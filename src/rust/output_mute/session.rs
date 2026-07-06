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
//! * `state == "opening"` / `state == "recording"` →
//!   [`MuteController::on_recording_start`]
//! * every terminal state (`transcribing`, `ready`, `no_text`,
//!   `cancelled`, `error`, `capture_lost`) →
//!   [`MuteController::on_recording_stop`]
//!
//! Codex P2 (session.rs:37, PR #440) — `opening` is treated as a start
//! transition rather than an ignored intermediate. The Python capture
//! path flips `self.recording = True`, emits `state="opening"`, opens
//! the capture stream, waits for first audio, and only then emits
//! `state="recording"`. Ignoring `opening` used to leak meeting audio
//! into the mic for the ~100-200 ms between "opening" and "recording";
//! muting on `opening` (which is idempotent w.r.t. a follow-up
//! `recording` event) closes that window.
//!
//! Codex P2 + Claude P2 (session.rs:32/37, PR #440) — `capture_lost`
//! counts as a terminal state. The Python capture path emits it
//! mid-recording when the microphone/pipe fails (`_arecord_reader` EOF,
//! reader-thread exception, mic unplugged) and does not follow up with
//! a further `transcribing`/`ready` transition; elsewhere in the
//! codebase (`src/rust/ui/worker_event.rs::worker_ready_for_state`) it
//! is already treated as "capture is over and the worker is effectively
//! back to ready". Without this fix, a mid-recording device loss left
//! the output muted indefinitely because our observer never saw a
//! terminal state.
//!
//! Intermediate lifecycle states that are neither a start-trigger nor a
//! terminal state (`post-processing`) are ignored so a slow
//! post-processor does not accidentally reset our saved prior state
//! mid-utterance.
//!
//! [`AppSettings::mute_output_while_recording`]: crate::config::AppSettings::mute_output_while_recording

use std::sync::{Mutex, OnceLock};

use crate::output_mute::{platform_backend, MuteController};

/// Env-var handle for the setting. Documented in `settings_schema.json`
/// as the fallback for users who prefer env-only configuration; the
/// runtime installer reads this when the on-disk config is silent.
///
/// Codex P2 (runtime.rs:2060, PR #440) — the runtime installer used to
/// read only config.json, so a user opting in via env alone was
/// silently ignored by the Rust supervisor. Exposed as `pub` so the
/// installer in `runtime.rs` uses the same string and a schema-key
/// rename fails at compile time rather than diverging silently.
pub const MUTE_OUTPUT_ENV: &str = "VOICEPI_MUTE_OUTPUT_WHILE_RECORDING";

/// Worker states that mean "recording is now over, restore any mute
/// we installed at recording start".
///
/// `capture_lost` is included because the Python capture path emits it
/// mid-recording when the microphone/pipe fails, without a subsequent
/// terminal transition. Treating it as ignored would leave the output
/// muted indefinitely on a mic unplug — see the module-level docs.
const TERMINAL_STATES: &[&str] = &[
    "transcribing",
    "ready",
    "no_text",
    "cancelled",
    "error",
    "capture_lost",
];

/// Worker states that mean "recording is about to begin, mute now".
///
/// `opening` fires before the capture stream is fully open — muting
/// here closes the ~100-200 ms window during which meeting/video audio
/// would otherwise leak into the mic buffer. The subsequent `recording`
/// event is a no-op because [`MuteController::on_recording_start`] is
/// idempotent while a recording is already in progress.
const START_STATES: &[&str] = &["opening", "recording"];

/// Worker states we deliberately ignore. They fall between the start
/// transition and a terminal state, and treating them as "stop" would
/// prematurely unmute during post-processing.
const IGNORED_STATES: &[&str] = &["post-processing"];

static CONTROLLER: OnceLock<Mutex<Option<MuteController>>> = OnceLock::new();

/// Monotonically-increasing observer generation. Bumped by every
/// [`install`] / [`install_test_controller`] so a stale
/// `stream_lines` reader from a stopped child can be told apart from
/// the current supervisor session.
///
/// Codex P2 (runtime.rs:2074, PR #440) — `Runtime::restart` calls
/// `stop()` (kills the previous child on a background thread) and
/// then `start()` immediately. The old per-child `stream_lines` thread
/// can still drain buffered worker events between those two calls.
/// Tagging each reader with the generation it was created under lets
/// [`observe_worker_state`] discard those stale events instead of
/// nudging the fresh controller into a bogus mute/unmute cycle.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn cell() -> &'static Mutex<Option<MuteController>> {
    CONTROLLER.get_or_init(|| Mutex::new(None))
}

/// Snapshot the current observer generation.
///
/// A `stream_lines` reader (or `EventForwarder`) should capture this
/// value once at construction and pass it to every
/// [`observe_worker_state`] call. When the supervisor stops/starts a
/// worker the generation is bumped, so any surviving reader from the
/// previous child sees `generation != current` and no-ops.
pub fn current_generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

fn bump_generation() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    // Codex P2 (runtime.rs:2074, PR #440) — every controller swap
    // bumps the observer generation so any stale reader from a
    // stopped child no-ops on its next `observe_worker_state` call.
    bump_generation();
}

/// Merge an on-disk config value with the [`MUTE_OUTPUT_ENV`] env var
/// and install (or clear) the controller accordingly.
///
/// Precedence matches Python's `vp_config.Config.effective_config`:
///
///   config.json (if explicitly set) -> environment -> default (off).
///
/// Codex P2 (session.rs:130, PR #440) — an earlier iteration let env
/// win unconditionally. That meant an operator who had exported
/// `VOICEPI_MUTE_OUTPUT_WHILE_RECORDING=1` could not disable the
/// feature by saving `"mute_output_while_recording": "0"` in Settings;
/// the Rust supervisor kept muting even after the effective runtime
/// config said "off". Config now wins over env so Settings can always
/// turn the feature off, matching the schema-driven Python resolution.
///
/// `config_value.is_none()` means "config silent on this key" (either
/// the file is absent or the key is missing entirely). `Some(x)` means
/// the user explicitly persisted `x` and the env var must NOT override
/// it. An unrecognised env value is treated as unset so a typo does
/// not silently flip the setting.
pub fn install_from_settings(config_value: Option<bool>) {
    let effective = config_value.or_else(env_override).unwrap_or(false);
    install(effective);
}

/// Parse [`MUTE_OUTPUT_ENV`] as a bool. `Some(true|false)` when set to
/// a recognised truthy/falsy token; `None` when unset or unparsable so
/// the caller can fall back to the config value.
///
/// Kept `pub(crate)` so the runtime installer can log which source won
/// without duplicating the parse. The token vocabulary matches the
/// `bool_value` parser used by config.json loading so env and config
/// accept identical strings.
pub(crate) fn env_override() -> Option<bool> {
    let raw = std::env::var(MUTE_OUTPUT_ENV).ok()?;
    parse_bool_env(&raw)
}

fn parse_bool_env(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
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
///
/// `observer_generation` is the value the caller captured via
/// [`current_generation`] when its reader was created. If a different
/// controller has been installed since (worker stop/start, config
/// hot-reload, test swap), this call is a cheap no-op — Codex P2
/// (runtime.rs:2074, PR #440) added this so a stale `stream_lines`
/// reader from a stopped child cannot drive the new supervisor
/// session's controller into a bogus mute/unmute cycle.
///
/// Returns `Some(MuteError)` when the observation triggered a backend
/// call that failed. Codex P2 (state.rs:158, PR #440) — the caller
/// (supervisor / rust-session forwarder) uses this to surface silent
/// backend failures (missing `pactl`, broken CoreAudio, PowerShell
/// spawn failure) to the user via the runtime log so the auto-mute
/// feature does not fail silently on every recording.
pub fn observe_worker_state(
    state: Option<&str>,
    observer_generation: u64,
) -> Option<MuteError> {
    let Some(state) = state else { return None };
    if IGNORED_STATES.contains(&state) {
        return None;
    }
    // Codex P2 (runtime.rs:2074, PR #440) — stale-generation guard.
    if observer_generation != current_generation() {
        return None;
    }
    let mut slot = cell().lock().unwrap_or_else(|e| e.into_inner());
    let Some(controller) = slot.as_mut() else {
        return None;
    };
    if START_STATES.contains(&state) {
        controller.on_recording_start();
    } else if TERMINAL_STATES.contains(&state) {
        controller.on_recording_stop();
    } else {
        return None;
    }
    controller.last_error().cloned()
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
    let previous = std::mem::replace(&mut *slot, controller);
    // Codex P2 (runtime.rs:2074, PR #440) — tests exercise the same
    // stale-generation guard, so a swap must bump too.
    bump_generation();
    previous
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
}
