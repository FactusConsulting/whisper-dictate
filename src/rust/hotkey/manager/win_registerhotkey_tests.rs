//! Companion tests for [`crate::hotkey::manager::win_registerhotkey`].
//!
//! Extracted from an inline `#[cfg(test)] mod tests` in
//! `win_registerhotkey.rs` so the regression-test discipline scanner
//! (per AGENTS.md `enforce-regression-test-discipline` — see
//! `src/tests/python/test_regression_test_discipline.py`) sees a matching
//! test file next to the production module.
//!
//! Every test here is `#[cfg(all(target_os = "windows", feature =
//! "rust-hotkeys"))]` because the production module is itself gated on
//! Windows + the feature flag; on Linux / macOS builds and on stock
//! Windows builds the whole file compiles to nothing and the test
//! harness sees zero tests. The `parse_chord` unit tests DO NOT touch
//! any Win32 API, so they run without a real hotkey install — the
//! Windows-only gate is only to keep the imports feature-consistent.

#![cfg(all(test, target_os = "windows", feature = "rust-hotkeys"))]

use crate::hotkey::manager::win_registerhotkey::{
    advance_state, is_ptt_hotkey_id, is_side_specific_modifier, parse_chord, plan_register,
    required_modifier_vk_groups, vk_from_trigger_name, LoopEmit, LoopState, LoopStimulus,
    ParsedChord, RegisterPlan, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

fn s(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn only_owned_message_id_can_drive_ptt() {
    assert!(is_ptt_hotkey_id(1));
    assert!(!is_ptt_hotkey_id(0));
    assert!(!is_ptt_hotkey_id(2));
}

#[path = "win_registerhotkey/chord_tests.rs"]
mod chord;
#[path = "win_registerhotkey/modifiers_tests.rs"]
mod modifiers;
#[path = "win_registerhotkey/registration_tests.rs"]
mod registration;
#[path = "win_registerhotkey/state_tests.rs"]
mod state;

// -----------------------------------------------------------------------
// The RegisterHotKey listener reports its thread lifetime through the shared
// `listener_alive` atomic so the hotkey self-test can detect a dead listener.
// -----------------------------------------------------------------------

/// Spawn the RegisterHotKey driver in-process and verify the alive
/// atomic flips from `true` to `false` when the message-loop thread
/// exits via the normal `Shutdown` path. This exercises the exact
/// wiring `self-test hotkey-boot --driver register` depends on.
///
/// Runs only on Windows because RegisterHotKey is a USER32 API. In
/// CI's headless Windows runner, `RegisterHotKey` might refuse an
/// already-owned chord — but the driver spawn itself doesn't
/// register anything until the caller sends a `Register` command, so
/// the spawn / shutdown lifecycle exercised here is independent of
/// chord availability.
#[test]
fn registerhotkey_listener_alive_flag_flips_on_thread_exit() {
    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::hotkey::manager::driver_common::NoopRawTap;
    use crate::hotkey::manager::win_registerhotkey::spawn_with_raw_tap;
    use std::sync::Arc;

    let guard = Arc::new(InjectionGuard::new());
    let (handle, thread) = match spawn_with_raw_tap(guard, |_out| {}, NoopRawTap) {
        Ok(pair) => pair,
        Err(err) => {
            // A sandboxed CI environment may refuse to spawn the bare thread
            // and message loop. Skip because there is no lifecycle to check.
            eprintln!(
                "skipping registerhotkey_listener_alive_flag_flips_on_thread_exit: \
                 spawn refused ({err}); no lifecycle to observe"
            );
            return;
        }
    };
    // After spawn, the listener flips the flag immediately before entering
    // `run_msg_loop`. Poll briefly so the assertion observes that transition.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    while std::time::Instant::now() < deadline && !handle.is_listener_alive() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        handle.is_listener_alive(),
        "immediately after spawn the RegisterHotKey msg-loop thread \
         should have reached its `store(true)` right before \
         run_msg_loop; is_listener_alive() must be true"
    );
    // Ask the message loop to exit cleanly.
    handle.shutdown();
    // Join the thread so we know the drop-guard / explicit store has
    // definitely run. `ManagerThread::join` blocks on the JoinHandle.
    thread.join();
    // Joining the thread completes synchronous message-loop teardown, so the
    // handler must have cleared the lifetime flag before this assertion.
    assert!(
        !handle.is_listener_alive(),
        "after shutdown + thread.join(), the RegisterHotKey listener \
         is definitely gone; is_listener_alive() must be false"
    );
}
