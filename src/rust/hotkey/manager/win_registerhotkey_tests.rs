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
    advance_state, dispatch_hotkey_message, is_copy_last_hotkey_id, is_paste_last_hotkey_id,
    is_ptt_hotkey_id, is_side_specific_modifier, parse_chord, plan_register,
    required_modifier_vk_groups, same_chord, vk_from_trigger_name, LoopEmit, LoopState,
    LoopStimulus, ParsedChord, RegisterPlan, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
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
fn paste_last_message_requires_its_own_registration_and_never_drives_ptt() {
    let mut state = LoopState::new();
    assert!(!is_paste_last_hotkey_id(3, &state));
    state.registered = Some(parse_chord(&s(&["pause"])).unwrap());
    state.paste_last_registered = Some(parse_chord(&s(&["ctrl", "f9"])).unwrap());
    assert!(is_paste_last_hotkey_id(3, &state));
    assert!(!is_ptt_hotkey_id(3));
    assert!(!is_paste_last_hotkey_id(1, &state));
    assert!(!is_paste_last_hotkey_id(2, &state));
    assert!(state.pressed_trigger.is_none());
}

#[test]
fn paste_last_message_emits_action_without_changing_ptt_state() {
    let mut state = LoopState::new();
    state.registered = Some(parse_chord(&s(&["pause"])).unwrap());
    state.paste_last_registered = Some(parse_chord(&s(&["ctrl", "f9"])).unwrap());
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&outputs);
    let on_output = Arc::new(move |output| received.lock().unwrap().push(output));

    dispatch_hotkey_message(3, &mut state, &on_output);
    dispatch_hotkey_message(99, &mut state, &on_output);
    assert_eq!(*outputs.lock().unwrap(), vec![TrackerOutput::PasteLast]);
    assert!(state.pressed_trigger.is_none());

    dispatch_hotkey_message(1, &mut state, &on_output);
    dispatch_hotkey_message(3, &mut state, &on_output);
    assert_eq!(
        *outputs.lock().unwrap(),
        vec![
            TrackerOutput::PasteLast,
            TrackerOutput::ChordPress,
            TrackerOutput::PasteLast,
        ]
    );
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
    // F12 is reserved by Windows and cannot demonstrate a second registration.
    let action = s(&["ctrl", "alt", "shift", "f10"]);
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
    assert!(
        result.is_ok(),
        "second hotkey registration failed: {result:?}"
    );
}

#[test]
fn registerhotkey_rejects_paste_last_collisions_and_registers_distinct_chords() {
    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::hotkey::manager::driver_common::NoopRawTap;
    use crate::hotkey::manager::win_registerhotkey::spawn_with_raw_tap;
    use std::sync::Arc;

    let (handle, thread) =
        spawn_with_raw_tap(Arc::new(InjectionGuard::new()), |_output| {}, NoopRawTap).unwrap();
    // Distinct chords from the copy-last dual-registration test: the two
    // live tests run in parallel threads of one process and RegisterHotKey
    // rejects a chord another thread already owns. F12 is reserved by
    // Windows and cannot demonstrate a second registration.
    let ptt = s(&["ctrl", "alt", "shift", "f9"]);
    let copy = s(&["ctrl", "alt", "shift", "f8"]);
    let paste = s(&["ctrl", "alt", "shift", "f7"]);
    if let Err(error) = handle.register(ptt.clone()) {
        handle.shutdown();
        thread.join();
        eprintln!("skipping triple-hotkey registration: PTT chord unavailable ({error})");
        return;
    }
    if let Err(error) = handle.register_copy_last(copy.clone()) {
        handle.unregister().unwrap();
        handle.shutdown();
        thread.join();
        eprintln!("skipping triple-hotkey registration: copy-last unavailable ({error})");
        return;
    }
    // paste == copy must be rejected before the OS registration is touched.
    let collision = handle.register_paste_last(copy.clone()).unwrap_err();
    assert!(collision.contains("copy-last and paste-last"));
    // The paste arm injects plain ctrl+v, and RegisterHotKey posts
    // WM_HOTKEY for synthetic key events, so that binding would
    // re-trigger its own paste. Rejected before any OS registration.
    let self_trigger = handle.register_paste_last(s(&["ctrl", "v"])).unwrap_err();
    assert!(self_trigger.contains("re-trigger"));
    let result = handle.register_paste_last(paste);
    handle.unregister().unwrap();
    handle.shutdown();
    thread.join();
    assert!(result.is_ok(), "paste-last registration failed: {result:?}");
}

#[test]
fn copy_last_owns_ctrl_v_only_until_paste_last_is_registered() {
    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::hotkey::manager::driver_common::NoopRawTap;
    use crate::hotkey::manager::win_registerhotkey::spawn_with_raw_tap;
    use std::sync::Arc;

    // Distinct chords from the other live tests (they run in parallel
    // threads of one process and RegisterHotKey rejects a chord another
    // thread already owns).
    let paste = s(&["ctrl", "alt", "shift", "f6"]);
    let copy_other = s(&["ctrl", "alt", "shift", "f5"]);
    let (handle, thread) =
        spawn_with_raw_tap(Arc::new(InjectionGuard::new()), |_output| {}, NoopRawTap).unwrap();
    // The startup order registers paste-last before copy-last, so the
    // paste arm never sees the conflicting copy binding here; the paste
    // arm's own guard is covered by the dedicated test below.
    handle.register_paste_last(paste.clone()).unwrap();
    // With paste-last registered, a ctrl+v copy binding would re-trigger
    // copy-last on every paste burst (Codex P2 win_registerhotkey.rs:572).
    let conflict = handle.register_copy_last(s(&["ctrl", "v"])).unwrap_err();
    assert!(conflict.contains("re-trigger copy-last"));
    // Symmetric PTT guard (Codex P2 win_registerhotkey.rs:620): with
    // paste-last registered, a ctrl+v PTT binding would start an
    // unintended recording on every paste burst.
    let ptt_conflict = handle.register(s(&["ctrl", "v"])).unwrap_err();
    assert!(ptt_conflict.contains("re-trigger PTT"));
    let result = handle.register_copy_last(copy_other);
    handle.unregister().unwrap();
    handle.shutdown();
    thread.join();
    assert!(
        result.is_ok(),
        "distinct copy-last registration failed: {result:?}"
    );
}

#[test]
fn paste_last_is_refused_while_ptt_owns_ctrl_v() {
    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::hotkey::manager::driver_common::NoopRawTap;
    use crate::hotkey::manager::win_registerhotkey::spawn_with_raw_tap;
    use std::sync::Arc;

    // Distinct chords from the other live tests (they run in parallel
    // threads of one process and RegisterHotKey rejects a chord another
    // thread already owns).
    let paste = s(&["ctrl", "alt", "shift", "f4"]);
    let (handle, thread) =
        spawn_with_raw_tap(Arc::new(InjectionGuard::new()), |_output| {}, NoopRawTap).unwrap();
    // Hand-edited config.json can bind PTT to ctrl+v first; the install
    // contends with the copy-last ctrl+v test's OS registration, so skip
    // rather than fail when the chord is already owned.
    if let Err(error) = handle.register(s(&["ctrl", "v"])) {
        handle.unregister().unwrap();
        handle.shutdown();
        thread.join();
        eprintln!("skipping PTT-owns-ctrl+v: registration unavailable ({error})");
        return;
    }
    // Enabling paste-last then would start an unintended recording on
    // every paste burst, so the paste arm refuses the registration
    // before the OS hotkey is touched (Codex P2
    // win_registerhotkey.rs:620).
    let conflict = handle.register_paste_last(paste).unwrap_err();
    assert!(conflict.contains("re-trigger PTT"));
    handle.unregister().unwrap();
    handle.shutdown();
    thread.join();
}

#[test]
fn paste_last_is_refused_while_copy_last_owns_ctrl_v() {
    use crate::hotkey::inject_guard::InjectionGuard;
    use crate::hotkey::manager::driver_common::NoopRawTap;
    use crate::hotkey::manager::win_registerhotkey::spawn_with_raw_tap;
    use std::sync::Arc;

    // Distinct chords from the other live tests (they run in parallel
    // threads of one process and RegisterHotKey rejects a chord another
    // thread already owns).
    let paste = s(&["ctrl", "alt", "shift", "f4"]);
    let (handle, thread) =
        spawn_with_raw_tap(Arc::new(InjectionGuard::new()), |_output| {}, NoopRawTap).unwrap();
    // Hand-edited config.json can register copy-last first; ctrl+v is
    // legitimate for copy-last while paste-last is absent.
    let copy = handle.register_copy_last(s(&["ctrl", "v"]));
    assert!(
        copy.is_ok(),
        "copy-last ctrl+v registration failed: {copy:?}"
    );
    // Enabling paste-last then would make every paste burst re-trigger
    // copy-last, so the paste arm refuses the registration before the
    // OS hotkey is touched (Codex P2 win_registerhotkey.rs:572).
    let conflict = handle.register_paste_last(paste).unwrap_err();
    assert!(conflict.contains("re-trigger copy-last"));
    handle.unregister().unwrap();
    handle.shutdown();
    thread.join();
}
