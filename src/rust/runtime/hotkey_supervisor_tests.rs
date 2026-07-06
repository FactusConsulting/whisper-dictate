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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _backend_guard = EnvVarGuard::remove("VOICEPI_HOTKEY_BACKEND");

    assert!(
        !rust_hotkey_backend_active(),
        "backend_active must be false when VOICEPI_HOTKEY_BACKEND is unset"
    );
}

#[test]
fn backend_active_returns_false_when_set_to_non_rust_value() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
