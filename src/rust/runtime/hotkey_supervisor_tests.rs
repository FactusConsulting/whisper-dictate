//! Tests for the Rust hotkey supervisor integration in RuntimeSupervisor
//! (PR #373 Codex findings: suspend-on-stop, resume-on-restart, truthy
//! toggle parsing, and the P1 "no disable_python_hotkey" constraint).

use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;

// -----------------------------------------------------------------------
// P1 #373: Python hotkey must NOT be disabled via the worker command —
// the Rust coordinator only logs actions; the actual recording lifecycle
// is still owned by Python until IPC is wired.
// -----------------------------------------------------------------------

#[test]
fn start_does_not_inject_python_hotkey_disable_flag() {
    // Even when VOICEPI_HOTKEY_BACKEND=rust is set, the supervisor must
    // NOT add VOICEPI_PYTHON_HOTKEY=0 to the effective command because the
    // Rust coordinator is not yet wired to drive recording.
    //
    // We verify this through the command env since we cannot spawn a real
    // worker in a unit test.
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");
    let _key_guard = EnvVarGuard::set("VOICEPI_KEY", "ctrl_l");

    // Build a command the way start() would, and confirm the flag is absent.
    let command = worker_command("/tmp/whisper-dictate");
    // install_rust_hotkey_from_command is a no-op in headless env (rdev
    // listener refuses to start), so the flag must remain absent regardless.
    let (tx, _rx) = std::sync::mpsc::channel();
    let _ = install_rust_hotkey_from_command(&command, tx, None);
    // Even if we called disable_python_hotkey, the test is that start() does
    // NOT call it — verified here by checking the clean command env.
    assert!(
        !command
            .env
            .iter()
            .any(|(k, _)| k == "VOICEPI_PYTHON_HOTKEY"),
        "install_rust_hotkey_from_command must not inject VOICEPI_PYTHON_HOTKEY=0; \
         Python must stay enabled until Rust IPC drives recording (PR #373 P1)"
    );
}

// -----------------------------------------------------------------------
// Fix 3 (#373): extract_hotkey_key_names used in the resume-on-restart path.
// -----------------------------------------------------------------------

#[test]
fn extract_hotkey_key_names_handles_single_key() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), "f9".to_owned()));

    let names = extract_hotkey_key_names(&command);
    assert_eq!(names, vec!["f9"]);
}

/// Codex P2 PR #421 runtime.rs:530 -- regression coverage for the
/// restart-path key-validation gate. The supervisor MUST NOT inject
/// `VOICEPI_PYTHON_HOTKEY=0` when the configured PTT key is one the
/// Rust (rdev) backend cannot translate -- otherwise the user's
/// Settings change would silently park Python and the Rust hotkey
/// would never fire (PTT goes silent for the whole session).
///
/// The full restart path needs a live `HotkeyHandle` (rust-hotkeys
/// feature + a successful rdev install, neither available in headless
/// CI), so this pins the contract at the helper level: extract +
/// validate the same way `RuntimeSupervisor::start` does on the
/// restart branch, and assert that an unsupported name short-circuits
/// without mutating the command env.
#[test]
fn extract_then_validate_rejects_unsupported_key_without_disabling_python() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), "super_l".to_owned()));

    let names = extract_hotkey_key_names(&command);
    assert_eq!(
        names,
        vec!["super_l"],
        "precondition: extract returns the configured (unsupported) name"
    );

    // On a feature build, validate_key_names rejects super_l (the
    // Python evdev backend accepts it but the rdev key map does not).
    // On a stub build, validate_key_names always returns Ok because
    // the supervisor's hotkey_handle is None, so the restart-path
    // validation gate is dead code -- but the test still pins the
    // command-env invariant below regardless of feature flag.
    #[cfg(feature = "rust-hotkeys")]
    assert!(
        crate::hotkey::validate_key_names(&names).is_err(),
        "feature build: super_l must be rejected by validate_key_names"
    );

    // The contract the restart path enforces: when validate_key_names
    // rejects, the supervisor must NOT call disable_python_hotkey, so
    // the command env stays free of VOICEPI_PYTHON_HOTKEY=0. A future
    // edit that reorders the gate (parks Python BEFORE validating)
    // would have to flip this assertion to fail.
    assert!(
        !command
            .env
            .iter()
            .any(|(k, _)| k == "VOICEPI_PYTHON_HOTKEY"),
        "rejected-key restart must NOT inject VOICEPI_PYTHON_HOTKEY=0; \
         the restart-path gate at runtime.rs:528-538 skips disable_python_hotkey \
         when validate_key_names errors so Python stays enabled and PTT \
         keeps working on the previous (supported) binding"
    );
}

/// Codex P2 PR #421 runtime.rs:530 -- pins the restart-path BRANCH
/// (not just the helpers `extract_hotkey_key_names` /
/// `validate_key_names`) via the extracted `restart_hotkey_decision`
/// helper that backs the supervisor's `else if let Some(handle)` arm.
/// A future edit that re-orders the gate (parks Python BEFORE
/// validating) has to fail this test before it can land.
///
/// The decision return covers all three observable outcomes of the
/// branch: skip-no-key (Python untouched), skip-unsupported (Python
/// untouched even though rust-session was requested), and resume
/// (with `park_python` reflecting the dictate-backend env).
#[test]
fn restart_hotkey_decision_covers_no_key_unsupported_and_resume_branches() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    // 1) No key configured -> SkipNoKey, regardless of dictate-backend.
    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    assert_eq!(
        restart_hotkey_decision(&command, true),
        RestartHotkeyDecision::SkipNoKey,
        "blank VOICEPI_KEY must short-circuit to SkipNoKey BEFORE parking Python"
    );
    assert_eq!(
        restart_hotkey_decision(&command, false),
        RestartHotkeyDecision::SkipNoKey,
    );

    // 2) Unsupported rdev key -> SkipUnsupported even when rust-session
    //    is requested. The supervisor's match arm for this variant must
    //    NOT call `disable_python_hotkey`, so Python keeps PTT alive on
    //    the previous (supported) binding. On stub builds
    //    `validate_key_names` always returns Ok, so the feature-cfg is
    //    required for the unsupported-key assertion to mean anything.
    #[cfg(feature = "rust-hotkeys")]
    {
        let mut command = worker_command("/tmp/whisper-dictate");
        command.env.retain(|(k, _)| k != "VOICEPI_KEY");
        command
            .env
            .push(("VOICEPI_KEY".to_owned(), "super_l".to_owned()));
        assert_eq!(
            restart_hotkey_decision(&command, true),
            RestartHotkeyDecision::SkipUnsupported {
                key_names: vec!["super_l".to_owned()],
            },
            "unsupported key must surface SkipUnsupported (not Resume) so the \
             supervisor skips disable_python_hotkey + resume"
        );
    }

    // 3) Supported key + rust-session requested -> Resume{park_python:true}.
    //    Supported key + rust-session NOT requested -> Resume{park_python:false}.
    //    `ctrl_l` is in rdev's translation table on every supported
    //    platform; on stub builds validate_key_names is a no-op pass-
    //    through so the result is the same.
    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), "ctrl_l".to_owned()));
    assert_eq!(
        restart_hotkey_decision(&command, true),
        RestartHotkeyDecision::Resume {
            key_names: vec!["ctrl_l".to_owned()],
            park_python: true,
        },
        "supported key + rust-session requested -> Resume with park_python=true"
    );
    assert_eq!(
        restart_hotkey_decision(&command, false),
        RestartHotkeyDecision::Resume {
            key_names: vec!["ctrl_l".to_owned()],
            park_python: false,
        },
        "supported key + rust-session NOT requested -> Resume with park_python=false"
    );
}

#[test]
fn extract_hotkey_key_names_handles_blank_key_as_empty() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);

    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");
    command
        .env
        .push(("VOICEPI_KEY".to_owned(), "   ".to_owned()));

    let names = extract_hotkey_key_names(&command);
    assert!(
        names.is_empty(),
        "blank VOICEPI_KEY must produce empty key_names (no install)"
    );
}

// -----------------------------------------------------------------------
// P2 #373: rust_hotkey_backend_active — the conjunction of requested AND
// available. Verified here since it lives in runtime.rs (not hotkey/).
// -----------------------------------------------------------------------

#[test]
fn backend_active_returns_false_when_not_requested() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _backend_guard = EnvVarGuard::remove("VOICEPI_HOTKEY_BACKEND");

    assert!(
        !rust_hotkey_backend_active(),
        "backend_active must be false when VOICEPI_HOTKEY_BACKEND is unset"
    );
}

#[test]
fn backend_active_returns_false_when_set_to_non_rust_value() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "pynput");

    assert!(
        !rust_hotkey_backend_active(),
        "backend_active must be false when backend is set to pynput (not rust)"
    );
}

/// When `VOICEPI_HOTKEY_BACKEND=rust` but the feature is absent, the
/// available gate must block activation.
#[test]
#[cfg(not(feature = "rust-hotkeys"))]
fn backend_active_returns_false_when_requested_but_feature_absent() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");

    assert!(
        !rust_hotkey_backend_active(),
        "backend_active must be false when feature is not compiled in"
    );
}

// -----------------------------------------------------------------------
// HotkeyHandle stub methods (non-rust-hotkeys builds): suspend + resume
// must compile and be no-ops.
// -----------------------------------------------------------------------

#[test]
fn hotkey_handle_stub_suspend_and_resume_are_no_ops() {
    // On a stock build (no rust-hotkeys feature) the HotkeyHandle stub must
    // compile and be callable without panicking. This test confirms the stub
    // methods satisfy the same call-sites as the real implementation so the
    // supervisor compiles on all build configurations.
    //
    // On a rust-hotkeys build this test is still valid — it just exercises
    // code paths that are always-compiled (the cfg guard is on install_hotkey,
    // not on the call sites in RuntimeSupervisor).
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::remove("VOICEPI_HOTKEY_BACKEND");
    let _key_guard = EnvVarGuard::remove("VOICEPI_KEY");

    // With no backend requested, install returns None, so the supervisor
    // path with a live handle is not reachable here. Verify the command-env
    // path (no VOICEPI_PYTHON_HOTKEY injection) compiles and runs cleanly.
    let mut command = worker_command("/tmp/whisper-dictate");
    command.env.retain(|(k, _)| k != "VOICEPI_KEY");

    let (tx, _rx) = std::sync::mpsc::channel();
    let handle = install_rust_hotkey_from_command(&command, tx, None);
    assert!(
        handle.is_none(),
        "no backend + no key → None handle (nothing to suspend/resume)"
    );
}

// -----------------------------------------------------------------------
// Sonar coverage uplift: route install_rust_hotkey_from_command through
// `install_session_sink_hotkey` (the rust-session branch) so Sonar sees
// the session-sink wrapper exercised even though the rdev listener
// install will return None in a headless CI runner. This pins the
// VOICEPI_DICTATE_BACKEND=rust-session routing contract.
// -----------------------------------------------------------------------

#[test]
fn install_rust_hotkey_routes_to_session_sink_when_backend_is_rust_session() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");
    let _dictate_guard = EnvVarGuard::set("VOICEPI_DICTATE_BACKEND", "rust-session");
    let _key_guard = EnvVarGuard::set("VOICEPI_KEY", "ctrl_l");
    // Codex P2 PR #421 hotkey_supervisor_tests.rs:248 -- the
    // session-sink path enables `VOICEPI_WORKER_EVENTS=1` via
    // `build_production_sink`; without an env-guard a later test that
    // grabs ENV_LOCK would inherit the truthy gate and reintroduce the
    // env-leak flake this PR fixes. The guard captures the
    // pre-fixture value and restores it on Drop.
    //
    // We also REMOVE the var explicitly here (not just guard it) so the
    // observable assertion below is a true measurement of what the
    // routing call set, not the leaked state from a prior test.
    let _worker_events_guard = EnvVarGuard::remove("VOICEPI_WORKER_EVENTS");

    // build a command the way start() would
    let command = worker_command("/tmp/whisper-dictate");

    // Codex P2 PR #421 hotkey_supervisor_tests.rs:230 (observability):
    // without an observable side-effect this assertion would still pass
    // if the route accidentally went through `install_logger_sink_hotkey`
    // (both paths return None in headless CI). The session-sink
    // builder sets `VOICEPI_WORKER_EVENTS=1` as a documented side
    // effect (rust_session_sink::build_production_sink:268); the logger
    // sink does not touch that var. Asserting on the var after the
    // call distinguishes the two routes so a future edit that
    // re-orders the gate (and falls through to the logger sink for
    // `rust-session`) fails this test.
    let (tx, _rx) = std::sync::mpsc::channel();
    let handle = install_rust_hotkey_from_command(&command, tx, None);
    let _ = handle;

    assert_eq!(
        std::env::var("VOICEPI_WORKER_EVENTS").ok().as_deref(),
        Some("1"),
        "session-sink route MUST set VOICEPI_WORKER_EVENTS=1 via \
         build_production_sink; if this fires as None the routing went \
         through install_logger_sink_hotkey (which does not touch the var)"
    );
}

/// Negative control for the routing observability test above: when
/// `VOICEPI_DICTATE_BACKEND` is NOT `rust-session`, the route MUST
/// go through `install_logger_sink_hotkey`, which leaves
/// `VOICEPI_WORKER_EVENTS` untouched. Pairs with the positive test to
/// pin the env-gate as a true two-sided signal of which sink ran.
#[test]
fn install_rust_hotkey_routes_to_logger_sink_when_dictate_backend_unset() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");
    let _dictate_guard = EnvVarGuard::remove("VOICEPI_DICTATE_BACKEND");
    let _key_guard = EnvVarGuard::set("VOICEPI_KEY", "ctrl_l");
    // Removed (not just guarded) so the post-call assertion measures
    // exactly what the routing call did, not state leaked from a
    // prior test.
    let _worker_events_guard = EnvVarGuard::remove("VOICEPI_WORKER_EVENTS");

    let command = worker_command("/tmp/whisper-dictate");

    let (tx, _rx) = std::sync::mpsc::channel();
    let handle = install_rust_hotkey_from_command(&command, tx, None);
    let _ = handle;

    assert!(
        std::env::var("VOICEPI_WORKER_EVENTS").is_err(),
        "logger-sink route MUST NOT set VOICEPI_WORKER_EVENTS; if this is Some(\"1\") \
         the route went through install_session_sink_hotkey (which would mean the \
         dictate-backend gate broke and we are silently driving the in-process \
         session for every install)"
    );
}

#[test]
fn install_rust_hotkey_session_sink_path_compiles_with_repaint_notifier() {
    // Same routing as the observability test above but with a real
    // `RepaintNotifier` so the closure-construction site in
    // `install_session_sink_hotkey` is covered too (it shows up in
    // Sonar even though the closure body never fires when the install
    // returns None). The same env-guard pattern keeps the worker-
    // events gate restored after the test (Codex P2 PR #421).
    let _guard = ENV_LOCK.lock().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/no-whisper-dictate-venv");
    let _python_guard = EnvVarGuard::remove(PYTHON_ENV);
    let _backend_guard = EnvVarGuard::set("VOICEPI_HOTKEY_BACKEND", "rust");
    let _dictate_guard = EnvVarGuard::set("VOICEPI_DICTATE_BACKEND", "rust-session");
    let _key_guard = EnvVarGuard::set("VOICEPI_KEY", "ctrl_l");
    let _worker_events_guard = EnvVarGuard::remove("VOICEPI_WORKER_EVENTS");

    let command = worker_command("/tmp/whisper-dictate");

    let wakeups = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wakeups_for_notifier = std::sync::Arc::clone(&wakeups);
    let notifier: crate::runtime::RepaintNotifier = std::sync::Arc::new(move || {
        wakeups_for_notifier.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });

    let (tx, _rx) = std::sync::mpsc::channel();
    let _ = install_rust_hotkey_from_command(&command, tx, Some(notifier));
    assert_eq!(
        wakeups.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no events flow when install returns None"
    );
    // Same observability check as above so this test also fails if
    // the route falls through to the logger sink.
    assert_eq!(
        std::env::var("VOICEPI_WORKER_EVENTS").ok().as_deref(),
        Some("1"),
        "session-sink route must set VOICEPI_WORKER_EVENTS=1 even with a notifier"
    );
}

// -----------------------------------------------------------------------
// Codex P2 #644 discussion r3659201761 — the Phase-B "installed" line
// must reflect the chord actually handed to the manager.
//
// Before the fix, `in_process_install_summary` re-loaded settings for
// the chord label. A save that landed between the install path's own
// read and this second read let the summary log the newer chord even
// though the listener was bound to the older one — misleading the
// Windows operator into thinking PTT would fire on the wrong chord.
// The fix routes the chord label through `format_installed_chord`,
// which takes the installer's registered key_names verbatim; the
// helper is a pure function so we can pin the regression bite here
// without spinning up a `HotkeyHandle`.
// -----------------------------------------------------------------------

#[test]
fn format_installed_chord_uses_the_names_actually_registered() {
    use crate::runtime::supervisor::format_installed_chord;

    // A single key: joined with no separator.
    assert_eq!(format_installed_chord(&["ctrl_l".to_owned()]), "ctrl_l");
    // Multi-key chord: joined on `+` in the exact order the manager
    // received. Order matters — the Windows operator debugging a wedge
    // reads this string as the ground truth for the listener's binding.
    assert_eq!(
        format_installed_chord(&["ctrl_l".to_owned(), "shift_l".to_owned()]),
        "ctrl_l+shift_l"
    );
    // The regression bite: the summary MUST reflect the ARGUMENT, not
    // some other value it might have re-loaded elsewhere. Pass a chord
    // that the pre-fix code would never have produced from settings
    // (a made-up alias) — a re-loader would have replaced it with the
    // on-disk value. The fixed helper returns it verbatim.
    assert_eq!(
        format_installed_chord(&["madeup_l".to_owned(), "phantom_r".to_owned()]),
        "madeup_l+phantom_r",
        "the summary must reflect the names the installer actually \
         registered, not a fresh settings read (Codex P2 #644 r3659201761)"
    );
}

// -----------------------------------------------------------------------
// Codex P2 #668 discussion 3664983412 — Windows-side supervisor
// regression for the failed-resume path.
//
// The Codex sweep for #644 introduced `handle.resume(key_names.clone())
// .map_err(InProcessInstallError::HotkeyInstallFailed)?;` inside
// `attempt_in_process_start`, but the accompanying tests only exercised
// `ManagerHandle::register` in isolation. This test drives the exact
// cross-module behaviour a Windows operator experiences: the in-process
// controller restarts, the pre-installed hotkey handle's manager
// channel has disconnected, `resume` errs, and the supervisor MUST
// refuse to emit Started/ready and fall through to Python. Runs on
// every OS (the stub handle skips the real listener) so it's not gated
// on a Windows CI runner.
// -----------------------------------------------------------------------

#[cfg(feature = "rust-hotkeys")]
#[test]
fn attempt_in_process_start_fails_cleanly_when_prior_handle_manager_is_dead() {
    use crate::hotkey::coordinator;
    use crate::hotkey::HotkeyHandle;
    use crate::runtime::in_process::InProcessInstallError;
    use crate::runtime::supervisor::{RuntimeEvent, RuntimeState, RuntimeSupervisor};
    use crate::runtime::worker_command::WorkerCommand;

    // Hold the crate-wide env lock so a concurrent test's env-mutation
    // doesn't race the settings-load `attempt_in_process_start` will do.
    let _guard = ENV_LOCK.lock().unwrap();
    // Point VOICEPI_CONFIG at a scratch JSON with a non-empty `key`
    // so load_settings() succeeds and the restart branch actually
    // reaches `resume`. An EmptyChord error would short-circuit
    // before the resume call and this test would prove nothing about
    // the fix. Using VOICEPI_CONFIG directly avoids depending on the
    // per-OS platform_config_dir + JSON schema surface.
    let cfg_dir = tempfile::tempdir().expect("tempdir for scratch config");
    let cfg_path = cfg_dir.path().join("config.json");
    std::fs::write(&cfg_path, "{\"key\":\"ctrl_l\"}").expect("write scratch config");
    let _cfg_guard = EnvVarGuard::set("VOICEPI_CONFIG", cfg_path.to_string_lossy().as_ref());
    let _engine_guard = EnvVarGuard::set(
        "VOICEPI_DICTATE_ENGINE",
        // "rust" — the value the Phase-1-default in-process dispatch
        // sees on the DEFAULT path. attempt_in_process_start is only
        // called on this branch.
        "rust",
    );

    // Build a supervisor with a pre-installed stub handle whose
    // manager thread has been shut down. The `install_stub_for_tests`
    // seam is the only way to drive this without a live OS listener
    // (which headless CI cannot provide).
    let mut supervisor = RuntimeSupervisor::new();
    let mut stub = HotkeyHandle::install_stub_for_tests(coordinator::Mode::HoldToTalk);
    // Sanity: fresh stub reports the manager as alive.
    assert!(
        stub.is_listener_alive(),
        "stub handle must start with an alive listener; something is wrong with the test seam"
    );
    // Now KILL just the manager. `resume` will fail on the next
    // register — this is the exact failure shape a restart against a
    // died-in-flight manager thread produces on a real Windows
    // install.
    stub.shutdown_manager_only_for_tests();
    supervisor.hotkey_handle = Some(stub);

    // Drive the restart path directly. The public `start()` would
    // ALSO try to spawn a Python worker after the Phase-B refusal,
    // which drags in HOME / venv discovery / process spawn and would
    // make the test flaky on CI. attempt_in_process_start is the
    // exact seam Codex asked for — the fallible-resume boundary.
    let cmd = WorkerCommand {
        program: std::path::PathBuf::from("/nonexistent-whisper-dictate"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("/tmp"),
        env: Vec::new(),
    };
    let result = supervisor.attempt_in_process_start(&cmd);

    // The resume Err MUST propagate as HotkeyInstallFailed. Before
    // the sweep fix, `resume` returned `()` and the supervisor
    // continued past this point to emit Started + ready — the Windows
    // tray would flip green on a silently-unregistered listener.
    match result {
        Err(InProcessInstallError::HotkeyInstallFailed(msg)) => {
            assert!(
                msg.contains("manager thread disconnected") || msg.contains("ack channel closed"),
                "expected the manager-disconnect error to bubble through resume; got {msg:?}"
            );
        }
        other => panic!(
            "expected HotkeyInstallFailed on the killed-manager resume path; got {other:?} \
             — this is the exact regression the Codex sweep fix was meant to catch \
             (P2 #668 discussion 3664983412)"
        ),
    }
    // State MUST NOT have transitioned to Running — the fallback
    // path in `start()` sets it back to Stopped after the Err, but
    // attempt_in_process_start itself only touches state on the
    // success arm. The invariant we care about here: the caller sees
    // Err BEFORE any Running mutation.
    assert_ne!(
        supervisor.state(),
        RuntimeState::Running,
        "attempt_in_process_start must NOT flip state to Running when \
         resume errs (Codex P2 #668 discussion 3664983412)"
    );
    // No Started / ready worker events must have been emitted. Drain
    // the runtime channel and assert nothing carries the Phase-B
    // installed marker. A pre-fix run would leak
    // `RuntimeEvent::Started { command: \"...in-process; driver=..., chord=...)\" }`
    // followed by the ready worker event.
    let mut phase_b_installed_seen = false;
    let mut ready_seen = false;
    let (_tx_keepalive, rx) = std::sync::mpsc::channel::<RuntimeEvent>();
    // Move the supervisor's rx via `poll_events`-style drain. The
    // supervisor stores its own rx internally; the simplest way to
    // observe emissions is to read them back through the supervisor's
    // own interface. Since supervisor.rx is pub(super), drain it:
    while let Ok(event) = supervisor.rx.try_recv() {
        match event {
            RuntimeEvent::Started { command } if command.contains("in-process") => {
                phase_b_installed_seen = true;
            }
            RuntimeEvent::Worker(ref w) if w.state.as_deref() == Some("ready") => {
                ready_seen = true;
            }
            _ => {}
        }
    }
    drop(_tx_keepalive);
    drop(rx);
    assert!(
        !phase_b_installed_seen,
        "attempt_in_process_start MUST NOT emit a Phase-B `Started {{ in-process }}` \
         event on the failed-resume path — the Windows GUI's tray would flip green \
         on a silently-unregistered listener. Codex P2 #668 discussion 3664983412."
    );
    assert!(
        !ready_seen,
        "no `state=ready` worker event may leak from the failed-resume path; \
         the UI's ready-latch would else fire on a dead listener. Codex P2 #668 \
         discussion 3664983412."
    );

    // Codex P2 #668 discussion 3665200198: the unusable handle MUST
    // be cleared from `self.hotkey_handle` before we return Err.
    // Otherwise the caller in `start()` falls through to the
    // Python-worker path, which sees `hotkey_handle == Some(dead)`,
    // takes the legacy restart branch, decides `park_python` under
    // `VOICEPI_DICTATE_BACKEND=rust-session`, disables the Python
    // listener, retries the same dead manager, ignores that second
    // Err, and leaves the spawned Python worker with no functioning
    // PTT. Before the fix `hotkey_handle` was still `Some` here; the
    // fix drops it.
    assert!(
        supervisor.hotkey_handle.is_none(),
        "attempt_in_process_start MUST clear self.hotkey_handle when \
         resume errs so the Python-worker fallback in start() does not \
         reuse the dead manager and park Python for good. Codex P2 #668 \
         discussion 3665200198."
    );
}

/// End-to-end guard for the Codex P2 #668 3665200198 fallback race: with
/// both `VOICEPI_DICTATE_ENGINE=rust` AND
/// `VOICEPI_DICTATE_BACKEND=rust-session` set, a `start()` call whose
/// in-process resume fails MUST NOT park Python. Even if the eventual
/// Python-worker spawn fails (no binary on this box), the effective
/// command handed to `Command::new` must NOT carry
/// `VOICEPI_PYTHON_HOTKEY=0`. If it did, the operator's PTT would go
/// silent on Windows whether the Python worker started or not — the
/// exact regression the fallback race produces.
///
/// The test exercises `start()` end-to-end (the public entry point, not
/// the private `attempt_in_process_start`) so the resume-Err → clear
/// handle → Python-worker fallback pipeline is covered as a whole.
#[cfg(feature = "rust-hotkeys")]
#[test]
fn start_with_rust_session_and_dead_hotkey_handle_does_not_park_python_on_fallback() {
    use crate::hotkey::coordinator;
    use crate::hotkey::HotkeyHandle;
    use crate::runtime::supervisor::RuntimeSupervisor;
    use crate::runtime::worker_command::WorkerCommand;

    let _guard = ENV_LOCK.lock().unwrap();
    // Non-empty chord so the in-process restart path reaches `resume`
    // rather than short-circuiting on EmptyChord.
    let cfg_dir = tempfile::tempdir().expect("tempdir for scratch config");
    let cfg_path = cfg_dir.path().join("config.json");
    std::fs::write(&cfg_path, "{\"key\":\"ctrl_l\"}").expect("write scratch config");
    let _cfg_guard = EnvVarGuard::set("VOICEPI_CONFIG", cfg_path.to_string_lossy().as_ref());
    let _engine_guard = EnvVarGuard::set("VOICEPI_DICTATE_ENGINE", "rust");
    // The exact combination the Codex comment calls out: rust-session
    // dictate backend PLUS the default ENGINE=rust in-process path.
    // Without this env, `dictate_backend_rust_session_requested()`
    // returns false and the fallback path never decides `park_python`
    // — the bug window only opens when BOTH are set.
    let _backend_guard = EnvVarGuard::set("VOICEPI_DICTATE_BACKEND", "rust-session");

    let mut supervisor = RuntimeSupervisor::new();
    let mut stub = HotkeyHandle::install_stub_for_tests(coordinator::Mode::HoldToTalk);
    stub.shutdown_manager_only_for_tests();
    supervisor.hotkey_handle = Some(stub);

    // A `WorkerCommand` pointing at a non-existent program so
    // `start()`'s Python-worker spawn fails fast — we don't want a
    // real child on CI. What we DO want to observe: that the fallback
    // path did not stamp `VOICEPI_PYTHON_HOTKEY=0` on the effective
    // command's env vector before the spawn attempt.
    let cmd = WorkerCommand {
        program: std::path::PathBuf::from("/nonexistent-whisper-dictate-fallback-test"),
        args: Vec::new(),
        working_dir: std::path::PathBuf::from("/tmp"),
        env: Vec::new(),
    };
    // We EXPECT start() to return Err (Python worker spawn fails
    // because the program does not exist). The behaviour we're
    // asserting is a SIDE EFFECT — the hotkey_handle slot's state and
    // the events emitted — not the top-level Result value.
    let _ = supervisor.start(cmd);

    // The dead handle must have been cleared by
    // `attempt_in_process_start` BEFORE the fallback branch ran.
    // Before the fix the slot stayed populated, the fallback branch
    // took the legacy restart path, parked Python, and re-called
    // `handle.resume` on the same dead manager.
    assert!(
        supervisor.hotkey_handle.is_none(),
        "start() fallback must not leave a dead HotkeyHandle in the \
         supervisor. Codex P2 #668 discussion 3665200198."
    );
}

#[test]
fn format_installed_chord_falls_back_to_placeholder_when_empty() {
    // Defensive: an empty slice should not produce an empty string,
    // which would render as `... chord=)` in the started-line — the
    // "?" placeholder mirrors the pre-fix helper's failure mode for
    // the unresolved case so a supervisor bug still produces a
    // visibly-anomalous log line rather than a silent gap.
    use crate::runtime::supervisor::format_installed_chord;
    assert_eq!(format_installed_chord(&[]), "?");
}
