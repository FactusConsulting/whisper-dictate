//! Process-global observer that ties the auto-mute controller to the
//! supervisor's worker-event stream.
//!
//! The supervisor reads worker events on a background thread in
//! `runtime.rs::stream_lines`; we do not want to plumb an `Arc` down
//! through every layer just to reach it, so this module owns a
//! `OnceLock<Mutex<Option<MuteController>>>` initialised at supervisor
//! startup when the [`AppSettings::mute_output_while_recording`] toggle
//! is on. `observe_worker_state` is a cheap no-op when the controller
//! is absent, which means the wiring is safe to leave permanently in
//! the event loop.
//!
//! The observer collapses the worker's state vocabulary into two
//! transitions:
//! * `state == "recording"` → [`MuteController::on_recording_start`]
//! * every other terminal state (`transcribing`, `ready`, `no_text`,
//!   `cancelled`, `error`) → [`MuteController::on_recording_stop`]
//!
//! Intermediate lifecycle states that are not "recording" and not
//! terminal (`opening`, `post-processing`) are ignored so a slow
//! post-processor does not accidentally reset our saved prior state
//! mid-utterance.
//!
//! [`AppSettings::mute_output_while_recording`]: crate::config::AppSettings::mute_output_while_recording

use std::sync::{Mutex, OnceLock};

use crate::output_mute::{platform_backend, MuteController};

/// Worker states that mean "recording is now over, restore any mute
/// we installed at recording start".
const TERMINAL_STATES: &[&str] = &["transcribing", "ready", "no_text", "cancelled", "error"];

/// Worker states we deliberately ignore. They fall between "recording"
/// and a terminal state, and treating them as "stop" would prematurely
/// unmute during post-processing.
const IGNORED_STATES: &[&str] = &["opening", "post-processing"];

static CONTROLLER: OnceLock<Mutex<Option<MuteController>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<MuteController>> {
    CONTROLLER.get_or_init(|| Mutex::new(None))
}

/// Install the process-global controller.
///
/// Called by the supervisor at start-up when the setting is on. Passing
/// `false` clears the controller so a later start-up (or a settings
/// hot-reload that turned the flag off) becomes a no-op again. Safe to
/// call more than once — the previous controller is dropped, which
/// restores any active mute in the process.
pub fn install(enabled: bool) {
    let mut slot = cell().lock().unwrap_or_else(|e| e.into_inner());
    if enabled {
        *slot = Some(MuteController::new(platform_backend()));
    } else {
        *slot = None;
    }
}

/// Whether a controller is currently installed. Useful for tests +
/// diagnostic surfaces (a future settings-tab "installed?" indicator).
pub fn is_installed() -> bool {
    let slot = cell().lock().unwrap_or_else(|e| e.into_inner());
    slot.is_some()
}

/// Feed a worker-event state string into the controller, if installed.
///
/// Case-sensitive on the token to match the worker's exact emission
/// (`vp_dictate.py` and `vp_capture.py` both emit lowercase kebab-case
/// tokens like `"post-processing"`). No-op when no controller is
/// installed, so leaving the call site permanently in `stream_lines`
/// costs one atomic + one mutex acquisition per worker event.
pub fn observe_worker_state(state: Option<&str>) {
    let Some(state) = state else { return };
    if IGNORED_STATES.contains(&state) {
        return;
    }
    let mut slot = cell().lock().unwrap_or_else(|e| e.into_inner());
    let Some(controller) = slot.as_mut() else {
        return;
    };
    if state == "recording" {
        controller.on_recording_start();
    } else if TERMINAL_STATES.contains(&state) {
        controller.on_recording_stop();
    }
}

/// Test-only helper: swap in a controller built from an arbitrary
/// backend. Returns the previous slot so the test can restore it and
/// avoid cross-test interference. See the session tests + integration
/// test for usage.
#[cfg(test)]
pub(crate) fn install_test_controller(
    controller: Option<MuteController>,
) -> Option<MuteController> {
    let mut slot = cell().lock().unwrap_or_else(|e| e.into_inner());
    std::mem::replace(&mut *slot, controller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    use crate::output_mute::{MuteError, OutputMuteBackend};
    // Private test lock so a panic in one session test cannot poison
    // the crate-wide ENV_LOCK (and cascade-fail every env-touching
    // test). The session tests do not read process env at all — they
    // only serialize on the process-global controller cell.
    static SESSION_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    #[derive(Default)]
    struct CountingBackend {
        muted: StdMutex<bool>,
        set_calls: StdMutex<Vec<bool>>,
    }

    impl OutputMuteBackend for CountingBackend {
        fn get_mute(&self) -> Result<bool, MuteError> {
            Ok(*self.muted.lock().unwrap())
        }
        fn set_mute(&self, muted: bool) -> Result<(), MuteError> {
            *self.muted.lock().unwrap() = muted;
            self.set_calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    #[test]
    fn observe_is_noop_without_installed_controller() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = install_test_controller(None);

        observe_worker_state(Some("recording"));
        observe_worker_state(Some("ready"));
        observe_worker_state(None);
        assert!(!is_installed());

        install_test_controller(saved);
    }

    #[test]
    fn recording_then_ready_drives_start_and_stop() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = Arc::new(CountingBackend::default());
        let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
        let saved = install_test_controller(Some(controller));

        observe_worker_state(Some("recording"));
        assert_eq!(*backend.set_calls.lock().unwrap(), vec![true]);
        observe_worker_state(Some("ready"));
        assert_eq!(*backend.set_calls.lock().unwrap(), vec![true, false]);

        install_test_controller(saved);
    }

    #[test]
    fn ignored_states_do_not_drive_transitions() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = Arc::new(CountingBackend::default());
        let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
        let saved = install_test_controller(Some(controller));

        // opening -> recording -> post-processing -> ready
        observe_worker_state(Some("opening"));
        assert!(backend.set_calls.lock().unwrap().is_empty());

        observe_worker_state(Some("recording"));
        observe_worker_state(Some("post-processing"));
        assert_eq!(
            *backend.set_calls.lock().unwrap(),
            vec![true],
            "post-processing must not unmute mid-utterance",
        );

        observe_worker_state(Some("ready"));
        assert_eq!(*backend.set_calls.lock().unwrap(), vec![true, false]);

        install_test_controller(saved);
    }

    #[test]
    fn install_replaces_previous_controller_and_restores_mute() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = Arc::new(CountingBackend::default());
        let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
        install_test_controller(Some(controller));

        observe_worker_state(Some("recording"));
        assert!(*backend.muted.lock().unwrap());

        // Replacing the controller drops the old one — Drop restores
        // the mute. Explicitly drop the returned previous slot before
        // asserting so the restore has already run.
        drop(install_test_controller(None));
        assert!(!*backend.muted.lock().unwrap(), "Drop must restore mute");
    }

    #[test]
    fn install_true_installs_and_install_false_clears() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = install_test_controller(None);

        install(true);
        assert!(is_installed());
        install(false);
        assert!(!is_installed());

        install_test_controller(saved);
    }

    #[test]
    fn cancelled_and_error_also_stop_recording() {
        // Belt-and-braces: every terminal state we listed must trigger
        // a stop, not just "ready". Regression guard for a future
        // reshuffle of TERMINAL_STATES.
        for terminal in ["transcribing", "no_text", "cancelled", "error"] {
            let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let backend = Arc::new(CountingBackend::default());
            let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
            let saved = install_test_controller(Some(controller));

            observe_worker_state(Some("recording"));
            observe_worker_state(Some(terminal));

            assert_eq!(
                *backend.set_calls.lock().unwrap(),
                vec![true, false],
                "terminal state {terminal:?} must restore mute",
            );

            install_test_controller(saved);
        }
    }
}
