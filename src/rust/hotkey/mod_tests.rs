//! Companion tests for `hotkey/mod.rs` — specifically the wiring of the
//! push-to-talk ownership guard into the install funnel.
//!
//! The guard's own behaviour is covered exhaustively in
//! `ptt_lock/mod_tests.rs`. What CANNOT be covered there is that
//! the shared `install_hotkey_with_context` funnel actually calls it, and calls it in the
//! right place — a real install needs an OS listener (an X display, a
//! Windows message pump) that headless CI does not have, so there is no
//! behavioural seam to drive.
//!
//! So this is a structural scanner, the same technique
//! `inject_guard_tests` and `diag_tests` use for their own
//! must-happen-before invariants. It is deliberately narrow: it asserts
//! the call exists and precedes the two spawns, and nothing about how the
//! rest of the function is written.

use crate::diag_tests::scan_fn_body;

/// The install funnel every backend and every entry point passes through.
const INSTALL_FN: &str = "fn install_hotkey_with_context<F, R, S>(";

#[test]
fn the_install_funnel_takes_push_to_talk_ownership() {
    // If this call is ever removed, two whisper-dictate processes can hold
    // the same chord again and inject over each other character by
    // character with nothing in either log -- the 2026-07-29 report. The
    // unit tests around `ptt_lock` would all still pass, because they test
    // the lock rather than its use, so the regression has to be caught
    // here.
    let body = scan_fn_body("src/rust/hotkey/mod.rs", INSTALL_FN);
    assert!(
        body.code.contains("ptt_lock::acquire_or_refuse("),
        "the shared hotkey install funnel must take push-to-talk ownership; \
         without it a second whisper-dictate process can register the same \
         chord and both will inject into the focused window at once."
    );
    assert!(
        body.code.contains("InstallError::AlreadyHeld"),
        "a refused acquisition must surface as `InstallError::AlreadyHeld` \
         so callers can tell it apart from the fallback-to-pynput errors."
    );
}

#[test]
fn ownership_is_taken_before_any_thread_is_spawned() {
    // Ordering is load-bearing twice over. A refused process must leave no
    // coordinator or listener thread behind, and the guard must sit ABOVE
    // driver selection -- the 2026-07-29 pair used two DIFFERENT drivers,
    // so a check pushed down into either one would be blind to the other.
    let body = scan_fn_body("src/rust/hotkey/mod.rs", INSTALL_FN);
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
        "push-to-talk ownership must be taken BEFORE the coordinator thread \
         spawns, so a refused install leaves nothing running"
    );
    assert!(
        acquire < manager,
        "push-to-talk ownership must be taken BEFORE the OS listener spawns, \
         and above driver selection -- a guard inside a driver cannot see a \
         second process using the other driver"
    );
}

#[test]
fn diagnostic_context_is_sampled_before_the_event_is_queued() {
    let body = scan_fn_body("src/rust/hotkey/mod.rs", INSTALL_FN);
    assert!(
        body.code
            .contains("bridge.send_with_context(event, source_context())"),
        "the OS-listener callback must sample focus into the same contextual \
         send that queues the coordinator event"
    );
    assert!(
        !body.code.contains("bridge.send(event)"),
        "a bare coordinator event would lose the event-source focus snapshot"
    );
}

#[test]
fn native_supervisor_surfaces_an_ownership_refusal_without_another_listener() {
    let body = scan_fn_body("src/rust/runtime/supervisor.rs", "pub fn start(&mut self,");
    assert!(
        body.code.contains("attempt_in_process_start"),
        "native start must go through the PTT ownership-guarded install"
    );
    assert!(
        !body.code.contains("Command::new(") && !body.code.contains("process.spawn("),
        "an ownership refusal must return as an error, never launch another listener"
    );
}

#[test]
fn the_handle_owns_the_lock_so_teardown_releases_push_to_talk() {
    // The lock has to live in `HotkeyHandle`, not in a local: that is what
    // ties its lifetime to the installed hotkey, so `Drop` (or process
    // death) hands push-to-talk back without an explicit release call
    // anywhere.
    let src = std::fs::read_to_string("src/rust/hotkey/mod.rs")
        .or_else(|_| std::fs::read_to_string("hotkey/mod.rs"))
        .expect("hotkey/mod.rs must be readable from the test working dir");
    let struct_start = src
        .find("pub struct HotkeyHandle {")
        .expect("HotkeyHandle must exist");
    let struct_end = src[struct_start..]
        .find("\n}")
        .map(|i| struct_start + i)
        .expect("HotkeyHandle must be brace-terminated");
    assert!(
        src[struct_start..struct_end].contains("ptt_lock: PttOwnershipState"),
        "`HotkeyHandle` must own the push-to-talk lock; holding it in a \
         local would release ownership the moment the install returns, \
         leaving the chord free for a second process to take."
    );
}

#[test]
fn begin_shutdown_releases_ptt_before_long_thread_joins() {
    let body = scan_fn_body("src/rust/hotkey/mod.rs", "pub(crate) fn begin_shutdown");
    let unregister = body
        .code
        .find("self.manager.unregister()")
        .expect("shutdown must unregister the active binding");
    let release = body
        .code
        .find("self.ptt_lock.release()")
        .expect("shutdown must synchronously release PTT ownership");
    let coordinator_shutdown = body
        .code
        .find("self.coordinator.shutdown()")
        .expect("shutdown must close the coordinator input");
    assert!(
        unregister < release && release < coordinator_shutdown,
        "PTT ownership must be released immediately after unregister and before \
         asynchronous coordinator teardown"
    );
}
