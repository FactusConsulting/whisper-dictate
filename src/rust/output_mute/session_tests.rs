//! Auto-mute observer + installer session-tests.
//!
//! Codex P2 (runtime.rs:2074, PR #440) — the session module gained
//! generation tracking and error surfacing; combined with the tests
//! it pushed session.rs over AGENTS.md's ~500-LOC modularity cap.
//! This sibling file houses the test suite; wired in via
//! `#[path = "session_tests.rs"] mod tests;` from `session.rs`.

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

    let _ = observe_worker_state(Some("recording"), current_generation());
    let _ = observe_worker_state(Some("ready"), current_generation());
    let _ = observe_worker_state(None, current_generation());
    assert!(!is_installed());

    install_test_controller(saved);
}

#[test]
fn recording_then_ready_drives_start_and_stop() {
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = Arc::new(CountingBackend::default());
    let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
    let saved = install_test_controller(Some(controller));

    let _ = observe_worker_state(Some("recording"), current_generation());
    assert_eq!(*backend.set_calls.lock().unwrap(), vec![true]);
    let _ = observe_worker_state(Some("ready"), current_generation());
    assert_eq!(*backend.set_calls.lock().unwrap(), vec![true, false]);

    install_test_controller(saved);
}

#[test]
fn ignored_states_do_not_drive_transitions() {
    // Codex P2 (session.rs:37, PR #440): only `post-processing` is
    // now an ignored intermediate. `opening` moved to START_STATES
    // (see `opening_starts_recording_and_recording_is_idempotent`).
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = Arc::new(CountingBackend::default());
    let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
    let saved = install_test_controller(Some(controller));

    // recording -> post-processing -> ready
    let _ = observe_worker_state(Some("recording"), current_generation());
    let _ = observe_worker_state(Some("post-processing"), current_generation());
    assert_eq!(
        *backend.set_calls.lock().unwrap(),
        vec![true],
        "post-processing must not unmute mid-utterance",
    );

    let _ = observe_worker_state(Some("ready"), current_generation());
    assert_eq!(*backend.set_calls.lock().unwrap(), vec![true, false]);

    install_test_controller(saved);
}

#[test]
fn opening_starts_recording_and_recording_is_idempotent() {
    // Codex P2 (session.rs:37, PR #440): opening must mute BEFORE
    // the mic buffer starts filling. The follow-up recording event
    // must not re-save state (idempotent), and the terminal state
    // must still restore the ORIGINAL prior mute value.
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = Arc::new(CountingBackend::default());
    let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
    let saved = install_test_controller(Some(controller));

    let _ = observe_worker_state(Some("opening"), current_generation());
    assert_eq!(
        *backend.set_calls.lock().unwrap(),
        vec![true],
        "opening must mute so meeting audio does not leak during the capture-open window",
    );

    // Idempotent: the immediately-following recording event must
    // NOT re-drive set_mute, otherwise a duplicate start would
    // overwrite the saved prior state with our own installed value.
    let _ = observe_worker_state(Some("recording"), current_generation());
    assert_eq!(
        *backend.set_calls.lock().unwrap(),
        vec![true],
        "recording after opening must be a no-op (idempotent start)",
    );

    let _ = observe_worker_state(Some("ready"), current_generation());
    assert_eq!(*backend.set_calls.lock().unwrap(), vec![true, false]);

    install_test_controller(saved);
}

#[test]
fn capture_lost_restores_mute() {
    // Codex P2 + Claude P2 (session.rs:32/37, PR #440): mid-recording
    // device loss emits capture_lost, not a normal terminal state.
    // Without this fix the controller stayed parked in Recording
    // forever and left the user's speakers muted until app exit.
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = Arc::new(CountingBackend::default());
    let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
    let saved = install_test_controller(Some(controller));

    let _ = observe_worker_state(Some("recording"), current_generation());
    let _ = observe_worker_state(Some("capture_lost"), current_generation());

    assert_eq!(
        *backend.set_calls.lock().unwrap(),
        vec![true, false],
        "capture_lost must restore the mute we installed at recording start",
    );

    install_test_controller(saved);
}

#[test]
fn config_wins_over_env_and_env_is_the_default_fallback() {
    // Codex P2 (session.rs:130, PR #440): precedence must mirror
    // Python's `vp_config.Config.effective_config`:
    // config.json (if explicitly set) -> environment -> default off.
    // The previous behaviour let env unconditionally override
    // config, so an operator with the env var set could not disable
    // the feature from Settings. Explicit Some(...) always wins now.
    use crate::test_env_lock::ENV_LOCK;
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(MUTE_OUTPUT_ENV).ok();

    // 1) explicit config=false MUST win over env=1.
    std::env::set_var(MUTE_OUTPUT_ENV, "1");
    assert_eq!(env_override(), Some(true));
    install_from_settings(Some(false));
    assert!(
        !is_installed(),
        "config=Some(false) must win over env=1 (Codex P2 session.rs:130)"
    );

    // 2) explicit config=true MUST win over env=0.
    std::env::set_var(MUTE_OUTPUT_ENV, "0");
    install_from_settings(Some(true));
    assert!(
        is_installed(),
        "config=Some(true) must win over env=0 (Codex P2 session.rs:130)"
    );

    // 3) config=None (key missing) -> env fallback: env=1 installs.
    std::env::set_var(MUTE_OUTPUT_ENV, "1");
    install_from_settings(None);
    assert!(is_installed(), "env=1 must apply when config is silent");

    // 4) config=None + env=0 -> not installed.
    std::env::set_var(MUTE_OUTPUT_ENV, "0");
    install_from_settings(None);
    assert!(
        !is_installed(),
        "env=0 must clear controller when config is silent"
    );

    // 5) Both silent -> default off.
    std::env::remove_var(MUTE_OUTPUT_ENV);
    assert_eq!(env_override(), None);
    install_from_settings(None);
    assert!(!is_installed(), "default is off when config + env are silent");

    // 6) Explicit config always wins even without env.
    install_from_settings(Some(true));
    assert!(is_installed());
    install_from_settings(Some(false));
    assert!(!is_installed());

    // Restore
    install_test_controller(None);
    match prev {
        Some(v) => std::env::set_var(MUTE_OUTPUT_ENV, v),
        None => std::env::remove_var(MUTE_OUTPUT_ENV),
    }
}

#[test]
fn env_override_recognises_truthy_and_falsy_tokens() {
    // Guard the token vocabulary so a future refactor cannot
    // silently narrow it.
    for truthy in ["1", "true", "TRUE", "yes", "on"] {
        assert_eq!(parse_bool_env(truthy), Some(true), "{truthy:?}");
    }
    for falsy in ["0", "false", "no", "off", ""] {
        assert_eq!(parse_bool_env(falsy), Some(false), "{falsy:?}");
    }
    for garbage in ["maybe", "2", "on!"] {
        assert_eq!(parse_bool_env(garbage), None, "{garbage:?}");
    }
}

#[test]
fn install_replaces_previous_controller_and_restores_mute() {
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = Arc::new(CountingBackend::default());
    let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
    install_test_controller(Some(controller));

    let _ = observe_worker_state(Some("recording"), current_generation());
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
    for terminal in [
        "transcribing",
        "no_text",
        "cancelled",
        "error",
        "capture_lost",
    ] {
        let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = Arc::new(CountingBackend::default());
        let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
        let saved = install_test_controller(Some(controller));

        let _ = observe_worker_state(Some("recording"), current_generation());
        let _ = observe_worker_state(Some(terminal), current_generation());

        assert_eq!(
            *backend.set_calls.lock().unwrap(),
            vec![true, false],
            "terminal state {terminal:?} must restore mute",
        );

        install_test_controller(saved);
    }
}
