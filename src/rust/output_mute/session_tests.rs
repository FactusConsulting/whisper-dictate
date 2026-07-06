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
    assert!(
        !is_installed(),
        "default is off when config + env are silent"
    );

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
fn read_mute_key_from_json_covers_every_precedence_rung() {
    // Claude P2 (runtime.rs:2279, PR #443) — the raw-JSON parser is
    // what makes "config wins over env, env fills in when config is
    // silent" actually work. The precedence integration test above
    // constructs Option<bool> directly and never exercises the JSON
    // parsing branches, so this table test pins each branch: bool,
    // recognised string tokens, unrecognised string (env fallback),
    // and missing key.
    use serde_json::{json, Map, Value};

    fn parse(v: Value) -> Option<bool> {
        match v {
            Value::Object(map) => read_mute_key_from_json(&map),
            _ => panic!("test helper expects an object"),
        }
    }

    // Bool branch.
    assert_eq!(
        parse(json!({ "mute_output_while_recording": true })),
        Some(true)
    );
    assert_eq!(
        parse(json!({ "mute_output_while_recording": false })),
        Some(false)
    );

    // Lax-string truthy tokens.
    for truthy in ["1", "true", "TRUE", "yes", "YES", "on", "  on  "] {
        assert_eq!(
            parse(json!({ "mute_output_while_recording": truthy })),
            Some(true),
            "truthy token {truthy:?} must parse as Some(true)",
        );
    }

    // Lax-string falsy tokens.
    for falsy in ["0", "false", "FALSE", "no", "off", ""] {
        assert_eq!(
            parse(json!({ "mute_output_while_recording": falsy })),
            Some(false),
            "falsy token {falsy:?} must parse as Some(false)",
        );
    }

    // Unrecognised string -> None so env fallback wins over a typo.
    for garbage in ["maybe", "2", "on!"] {
        assert_eq!(
            parse(json!({ "mute_output_while_recording": garbage })),
            None,
            "unrecognised token {garbage:?} must parse as None (env fallback)",
        );
    }

    // Numeric / null / array -> None (unsupported shape).
    assert_eq!(parse(json!({ "mute_output_while_recording": 1 })), None);
    assert_eq!(parse(json!({ "mute_output_while_recording": null })), None);
    assert_eq!(parse(json!({ "mute_output_while_recording": [] })), None);

    // Missing key -> None so env fallback wins.
    let empty: Map<String, Value> = Map::new();
    assert_eq!(read_mute_key_from_json(&empty), None);

    // Sibling keys must not confuse the parser.
    assert_eq!(
        parse(json!({ "unrelated_key": true, "mute_output_while_recording": true })),
        Some(true)
    );
}

#[test]
fn observe_worker_state_rejects_stale_observer_generation() {
    // Claude P2 (session.rs:235, PR #443) — the stale-reader guard is
    // the point of the generation-tag mechanism and previously had
    // ZERO coverage: every session test above passes
    // `current_generation()` itself, which always matches. This test
    // exercises the "observer's generation differs from the current
    // one" branch that used to be a silent dead code path.
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = Arc::new(CountingBackend::default());
    let controller = MuteController::new(backend.clone() as Arc<dyn OutputMuteBackend>);
    let saved = install_test_controller(Some(controller));

    // Snapshot the CURRENT generation as a "stale" reader would have
    // done at worker start.
    let stale_generation = current_generation();

    // Simulate a worker restart: install() bumps the generation.
    install(true);
    assert_ne!(
        stale_generation,
        current_generation(),
        "install() must bump the generation so stale readers can be filtered",
    );

    // Feed a "recording" event with the STALE generation. The guard
    // must reject it before it drives the freshly-installed controller.
    // Get the fresh backend's mute state by pulling the underlying
    // recorded set_calls — the CountingBackend built above is now
    // dropped, so we assert via the freshly-installed controller's
    // is_installed + a lack of side-effects on the ORIGINAL backend.
    let _ = observe_worker_state(Some("recording"), stale_generation);
    assert!(
        backend.set_calls.lock().unwrap().is_empty(),
        "stale-generation event must NOT drive the newly-installed controller",
    );

    // Sanity: a call with the CURRENT generation still drives the
    // installed controller (proves the guard is not a blanket no-op).
    let _ = observe_worker_state(Some("recording"), current_generation());

    install_test_controller(saved);
}

#[test]
fn install_from_settings_hot_swaps_without_bumping_generation() {
    // Codex P2 (session.rs:230, PR #443) — the settings UI Save/Reload
    // paths must swap the controller WITHOUT bumping the observer
    // generation, so the live worker's `stream_lines` reader keeps
    // driving the new controller. Plain `install()` bumps; the hot
    // variant does not.
    let _lock = SESSION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = install_test_controller(None);

    // Baseline generation captured as a live reader would have done.
    let gen_before = current_generation();

    // Hot-swap ON with a Some(true) config value.
    install_from_settings_hot(Some(true));
    assert!(is_installed(), "hot swap ON must install the controller");
    assert_eq!(
        gen_before,
        current_generation(),
        "hot swap must NOT bump generation (Codex P2 session.rs:230 PR #443)",
    );

    // Hot-swap OFF via Some(false).
    install_from_settings_hot(Some(false));
    assert!(!is_installed(), "hot swap OFF must clear the controller");
    assert_eq!(
        gen_before,
        current_generation(),
        "hot swap OFF must also NOT bump generation",
    );

    // A live reader that captured `gen_before` still matches, so a
    // follow-up observation drives whatever is installed now (nothing
    // in this branch — clean no-op).
    let _ = observe_worker_state(Some("recording"), gen_before);
    let _ = observe_worker_state(Some("ready"), gen_before);
    assert!(!is_installed());

    install_test_controller(saved);
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
