//! Companion tests for `hotkey/install.rs`'s stubs and the Rust-hotkey
//! backend selection gates
//! ([`rust_hotkey_backend_requested`]/[`rust_hotkey_backend_available`]),
//! so the gate and its pins stay in one module.

use crate::diag_tests::scan_fn_body;
use crate::hotkey::config::HotkeyConfig;
#[cfg(not(feature = "rust-hotkeys"))]
use crate::hotkey::config::InstallError;
use crate::hotkey::coordinator;
#[cfg(not(feature = "rust-hotkeys"))]
use crate::hotkey::install::install_hotkey;

/// The install funnel every backend and every entry point passes through.
const INSTALL_FN: &str = "fn install_hotkey_with_context<F, R, S>(";

// -----------------------------------------------------------------------
// HotkeyConfig constructor and stub install_hotkey (non-feature builds).
// -----------------------------------------------------------------------

/// `HotkeyConfig::hold_to_talk` is the primary public constructor; verify
/// that it populates both fields correctly. This test runs on all build
/// configurations (the constructor is not feature-gated).
#[test]
fn hold_to_talk_constructor_sets_key_names_and_mode() {
    let keys = vec!["ctrl_l".to_owned(), "f9".to_owned()];
    let cfg = HotkeyConfig::hold_to_talk(keys.clone());
    assert_eq!(cfg.key_names, keys);
    assert!(
        matches!(cfg.mode, coordinator::Mode::HoldToTalk),
        "hold_to_talk constructor must set Mode::HoldToTalk"
    );
}

/// On a stock build (no `rust-hotkeys` feature) `install_hotkey` must
/// immediately return `InstallError::Unsupported` so the supervisor can
/// log a one-line warning and keep the pynput path active. No threads
/// are spawned and no OS resources are acquired.
#[test]
#[cfg(not(feature = "rust-hotkeys"))]
fn install_hotkey_stub_returns_unsupported_error() {
    let cfg = HotkeyConfig::hold_to_talk(vec!["ctrl_l".to_owned()]);
    let err = match install_hotkey(cfg, |_| {}) {
        Ok(_) => panic!("stub install_hotkey must never return Ok"),
        Err(e) => e,
    };
    assert!(
        matches!(err, InstallError::Unsupported),
        "stock build must return InstallError::Unsupported, got: {err}"
    );
}

/// On a stock build (no `rust-hotkeys` feature) `validate_key_names` is
/// a no-op that always returns `Ok(())` -- the supervisor's
/// `hotkey_handle` is None in stub builds so the restart-path
/// validation gate (runtime.rs) is dead code, but the function itself
/// must compile and return Ok for any input so the call site type-
/// checks under both feature configurations.
///
/// Without this test the 3-line stub body is uncovered in the Sonar
/// build (which uses `--features ui-egui-glow`, NOT `rust-hotkeys`),
/// which the new-code coverage gate would flag.
// -----------------------------------------------------------------------
// Install-funnel structural scanners.
//
// The guard's own behaviour is covered exhaustively in
// `ptt_lock/mod_tests.rs`. What CANNOT be covered there is that the shared
// `install_hotkey_with_context` funnel actually calls it, and calls it in
// the right place -- a real install needs an OS listener (an X display, a
// Windows message pump) that headless CI does not have, so there is no
// behavioural seam to drive. So these are structural scanners, the same
// technique `inject_guard_tests` and `diag_tests` use for their own
// must-happen-before invariants.
// -----------------------------------------------------------------------

#[test]
fn the_install_funnel_takes_push_to_talk_ownership() {
    // If this call is ever removed, two whisper-dictate processes can hold
    // the same chord again and inject over each other character by
    // character with nothing in either log -- the 2026-07-29 report. The
    // unit tests around `ptt_lock` would all still pass, because they test
    // the lock rather than its use, so the regression has to be caught
    // here.
    let body = scan_fn_body("src/rust/hotkey/install.rs", INSTALL_FN);
    assert!(
        body.code.contains("ptt_lock::acquire_or_refuse("),
        "the shared hotkey install funnel must take push-to-talk ownership;          without it a second whisper-dictate process can register the same          chord and both will inject into the focused window at once."
    );
    assert!(
        body.code.contains("InstallError::AlreadyHeld"),
        "a refused acquisition must surface as `InstallError::AlreadyHeld`          so callers can tell it apart from the fallback-to-pynput errors."
    );
}

#[test]
fn ownership_is_taken_before_any_thread_is_spawned() {
    // Ordering is load-bearing twice over. A refused process must leave no
    // coordinator or listener thread behind, and the guard must sit ABOVE
    // driver selection -- the 2026-07-29 pair used two DIFFERENT drivers,
    // so a check pushed down into either one would be blind to the other.
    let body = scan_fn_body("src/rust/hotkey/install.rs", INSTALL_FN);
    let acquire = body
        .code
        .find("ptt_lock::acquire_or_refuse(")
        .expect("the ownership acquisition must exist");
    let coordinator = body
        .code
        .find("spawn_coordinator_with_context(")
        .expect("the coordinator spawn must exist");
    let manager = body
        .code
        .find("spawn_manager_with_driver(")
        .expect("the manager spawn must exist");
    assert!(
        acquire < coordinator,
        "push-to-talk ownership must be taken BEFORE the coordinator thread          spawns, so a refused install leaves nothing running"
    );
    assert!(
        acquire < manager,
        "push-to-talk ownership must be taken BEFORE the OS listener spawns,          and above driver selection -- a guard inside a driver cannot see a          second process using the other driver"
    );
}

#[test]
fn diagnostic_context_is_sampled_before_the_event_is_queued() {
    let body = scan_fn_body("src/rust/hotkey/install.rs", INSTALL_FN);
    assert!(
        body.code
            .contains("bridge.send_with_context(event, source_context())"),
        "the OS-listener callback must sample focus into the same contextual          send that queues the coordinator event"
    );
    assert!(
        !body.code.contains("bridge.send(event)"),
        "a bare coordinator event would lose the event-source focus snapshot"
    );
}
