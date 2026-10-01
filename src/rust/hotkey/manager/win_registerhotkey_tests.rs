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

use crate::hotkey::manager::tracker::TrackerOutput;
use crate::hotkey::manager::win_registerhotkey::{
    advance_state, dispatch_hotkey_message, is_copy_last_hotkey_id, is_ptt_hotkey_id,
    is_side_specific_modifier, parse_chord, plan_register, required_modifier_vk_groups, same_chord,
    vk_from_trigger_name, LoopEmit, LoopState, LoopStimulus, ParsedChord, RegisterPlan, MOD_ALT,
    MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};
use std::sync::{Arc, Mutex};

fn s(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn only_owned_message_id_can_drive_ptt() {
    assert!(is_ptt_hotkey_id(1));
    assert!(!is_ptt_hotkey_id(0));
    assert!(!is_ptt_hotkey_id(2));
}

#[test]
fn copy_last_message_requires_its_own_registration_and_never_drives_ptt() {
    let mut state = LoopState::new();
    assert!(!is_copy_last_hotkey_id(2, &state));
    state.registered = Some(parse_chord(&s(&["pause"])).unwrap());
    state.copy_last_registered = Some(parse_chord(&s(&["ctrl", "f8"])).unwrap());
    assert!(is_copy_last_hotkey_id(2, &state));
    assert!(!is_ptt_hotkey_id(2));
    assert!(!is_copy_last_hotkey_id(1, &state));
    assert!(state.pressed_trigger.is_none());
}

#[test]
fn copy_last_message_emits_action_without_changing_ptt_state() {
    let mut state = LoopState::new();
    state.registered = Some(parse_chord(&s(&["pause"])).unwrap());
    state.copy_last_registered = Some(parse_chord(&s(&["ctrl", "f8"])).unwrap());
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&outputs);
    let on_output = Arc::new(move |output| received.lock().unwrap().push(output));

    dispatch_hotkey_message(2, &mut state, &on_output);
    dispatch_hotkey_message(99, &mut state, &on_output);
    assert_eq!(*outputs.lock().unwrap(), vec![TrackerOutput::CopyLast]);
    assert!(state.pressed_trigger.is_none());

    dispatch_hotkey_message(1, &mut state, &on_output);
    assert_eq!(
        state.pressed_trigger,
        state.registered.as_ref().map(|chord| chord.vk)
    );
    dispatch_hotkey_message(2, &mut state, &on_output);
    assert_eq!(
        *outputs.lock().unwrap(),
        vec![
            TrackerOutput::CopyLast,
            TrackerOutput::ChordPress,
            TrackerOutput::CopyLast,
        ]
    );
    assert_eq!(
        state.pressed_trigger,
        state.registered.as_ref().map(|chord| chord.vk)
    );
}

#[test]
fn action_chord_collision_compares_windows_modifiers_and_virtual_key() {
    let ptt = parse_chord(&s(&["win", "f8"])).unwrap();
    let alias = parse_chord(&s(&["cmd", "f8"])).unwrap();
    let distinct = parse_chord(&s(&["win", "f9"])).unwrap();
    assert!(same_chord(&ptt, &alias));
    assert!(!same_chord(&ptt, &distinct));
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

#[test]
fn registerhotkey_can_own_ptt_and_copy_last_on_one_listener() {
    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::hotkey::manager::driver_common::NoopRawTap;
    use crate::hotkey::manager::win_registerhotkey::spawn_with_raw_tap;
    use std::sync::Arc;

    let (handle, thread) =
        spawn_with_raw_tap(Arc::new(InjectionGuard::new()), |_output| {}, NoopRawTap).unwrap();
    let ptt = s(&["ctrl", "alt", "shift", "f11"]);
    let action = s(&["ctrl", "alt", "shift", "f12"]);
    if let Err(error) = handle.register(ptt.clone()) {
        handle.shutdown();
        thread.join();
        eprintln!("skipping dual-hotkey registration: PTT chord unavailable ({error})");
        return;
    }
    assert!(handle
        .register_copy_last(ptt)
        .unwrap_err()
        .contains("must differ"));
    let result = handle.register_copy_last(action);
    handle.unregister().unwrap();
    handle.shutdown();
    thread.join();
    if let Err(error) = result {
        assert!(
            error.contains("GetLastError=0x00000581"),
            "second hotkey registration failed unexpectedly: {error}"
        );
        eprintln!("skipping occupied copy-last chord: {error}");
    }
}
