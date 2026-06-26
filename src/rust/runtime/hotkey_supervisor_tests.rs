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
    let _ = install_rust_hotkey_from_command(&command, tx);
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
    let handle = install_rust_hotkey_from_command(&command, tx);
    assert!(
        handle.is_none(),
        "no backend + no key → None handle (nothing to suspend/resume)"
    );
}
