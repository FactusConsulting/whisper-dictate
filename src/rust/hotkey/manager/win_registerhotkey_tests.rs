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
    advance_state, is_side_specific_modifier, parse_chord, plan_register,
    required_modifier_vk_groups, vk_from_trigger_name, LoopEmit, LoopState, LoopStimulus,
    ParsedChord, RegisterPlan, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

fn s(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn parses_function_key_alone_with_no_modifiers() {
    let parsed = parse_chord(&s(&["f9"])).expect("f9 parses");
    assert_eq!(parsed.mods, 0);
    assert_eq!(parsed.vk, 0x78);
    assert_eq!(parsed.trigger_name, "f9");
    assert_eq!(parsed.display, "f9");
}

#[test]
fn parses_ctrl_plus_function_key() {
    // The most common Windows-friendly chord: one generic modifier + one
    // trigger. Side-specific `ctrl_l` is rejected (see
    // `rejects_side_specific_modifier_aliases` below) because the OS
    // `MOD_*` flags fire on either side — the caller must use the
    // generic name for the RegisterHotKey driver.
    let parsed = parse_chord(&s(&["ctrl", "f9"])).expect("ctrl+f9 parses");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
}

#[test]
fn parses_all_four_modifier_families_with_trigger() {
    // Every MOD_* flag must OR into the mask; the trigger stays a
    // single VK. The permutation catches an accidental single-family
    // reset in `parse_chord` (each mod bit is independent).
    let parsed = parse_chord(&s(&["ctrl", "shift", "alt", "cmd", "f10"])).expect("parses");
    assert_eq!(parsed.mods, MOD_CONTROL | MOD_SHIFT | MOD_ALT | MOD_WIN);
    assert_eq!(parsed.vk, 0x79);
}

#[test]
fn generic_alt_maps_to_mod_alt() {
    // Side-specific alt aliases are rejected up-front (see
    // `rejects_side_specific_modifier_aliases`); the generic `alt`
    // still maps to MOD_ALT so users who don't care which side is
    // pressed get a valid install.
    let parsed = parse_chord(&s(&["alt", "f9"])).expect("alt+f9 parses");
    assert_eq!(parsed.mods, MOD_ALT);
}

#[test]
fn generic_win_and_cmd_names_map_to_mod_win() {
    // The tracker names the Windows key as `cmd` (macOS vocabulary —
    // inherited from pynput); the RegisterHotKey flag is MOD_WIN. Both
    // generic `cmd` and the friendlier generic `win` names are accepted;
    // the sided variants (`cmd_l`, `win_r`, …) are rejected up-front
    // because the OS flag fires on either side.
    for win_name in ["cmd", "win"] {
        let parsed = parse_chord(&s(&[win_name, "f9"]))
            .unwrap_or_else(|e| panic!("win alias {win_name:?} must parse: {e}"));
        assert_eq!(parsed.mods, MOD_WIN);
    }
}

#[test]
fn rejects_side_specific_modifier_aliases() {
    // `MOD_CONTROL` fires on
    // EITHER Ctrl side, so registering `ctrl_r+f9` would also match
    // `ctrl_l+f9` — the opposite of what the user configured. The
    // driver rejects side-specific aliases at parse time so the
    // supervisor falls back to rdev (which tracks sides accurately).
    //
    // The full list mirrors `is_side_specific_modifier`: two per family
    // (Ctrl / Shift / Alt / Win / Cmd) plus the right-Alt aliases the
    // rdev driver accepts (`alt_gr`, `right_alt`, `ralt`) since those
    // all name the right-Alt key specifically.
    for sided in [
        "ctrl_l",
        "ctrl_r",
        "shift_l",
        "shift_r",
        "alt_l",
        "alt_r",
        "alt_gr",
        "right_alt",
        "ralt",
        "cmd_l",
        "cmd_r",
        "win_l",
        "win_r",
    ] {
        let err = parse_chord(&s(&[sided, "f9"]))
            .expect_err(&format!("side-specific alias {sided:?} must be rejected"));
        assert!(
            err.contains("side-specific") || err.contains(sided),
            "message must name the constraint or the offending alias: {err} (alias={sided:?})"
        );
        assert!(
            err.contains("rdev") || err.contains("generic"),
            "message must point at the escape hatch (rdev fallback / generic name): {err}"
        );
    }
}

#[test]
fn is_side_specific_modifier_matches_the_reject_set() {
    // The rejection list must stay in sync with the helper used to test
    // it (parse_chord itself uses the helper), so cover the helper
    // directly here as well.
    for sided in [
        "ctrl_l",
        "ctrl_r",
        "shift_l",
        "shift_r",
        "alt_l",
        "alt_r",
        "alt_gr",
        "right_alt",
        "ralt",
        "cmd_l",
        "cmd_r",
        "win_l",
        "win_r",
    ] {
        assert!(
            is_side_specific_modifier(sided),
            "{sided:?} should be side-specific"
        );
    }
    for generic in ["ctrl", "shift", "alt", "cmd", "win"] {
        assert!(
            !is_side_specific_modifier(generic),
            "generic {generic:?} must NOT be side-specific"
        );
    }
    // Trigger names should never register as side-specific modifiers.
    for trig in ["f9", "space", "esc", "a", "1"] {
        assert!(!is_side_specific_modifier(trig));
    }
}

#[test]
fn rejects_modifier_only_chord_with_actionable_message() {
    // The signature limitation of RegisterHotKey: modifier-only chords
    // are NOT supported. The error message must name the constraint
    // AND point the user at the escape hatch (VOICEPI_HOTKEY_DRIVER=rdev)
    // so a user seeing this in the diagnostic log can fix it without
    // reading source.
    let err = parse_chord(&s(&["ctrl"])).expect_err("bare modifier must be rejected");
    assert!(
        err.contains("only modifiers") || err.contains("modifier"),
        "message must call out the modifier-only constraint: {err}"
    );
    assert!(
        err.contains("rdev") || err.contains("trigger"),
        "message must point at the escape hatch: {err}"
    );
}

#[test]
fn rejects_multiple_modifier_only_binding() {
    // Multiple modifiers with no trigger: same limitation, same
    // rejection. Guards against a future refactor that would treat
    // "N modifiers, N > 1" as "chord present".
    let err = parse_chord(&s(&["ctrl", "shift"])).expect_err("bare modifiers must be rejected");
    assert!(err.contains("modifier"), "{err}");
}

#[test]
fn rejects_two_non_modifier_triggers() {
    // RegisterHotKey binds ONE trigger VK per hotkey id. A chord with
    // two triggers (e.g. `f9+f10`) is a config error we surface up-
    // front rather than silently registering only the first.
    let err = parse_chord(&s(&["f9", "f10"])).expect_err("two triggers must be rejected");
    assert!(err.contains("more than one"), "{err}");
    // Both names must appear so the user can see which two collided.
    assert!(err.contains("f9") && err.contains("f10"), "{err}");
}

#[test]
fn rejects_unsupported_trigger_name() {
    // Anything outside f1..f12, ASCII letter/digit, space/esc/tab/enter/pause
    // is rejected with a message that names the offender. Guards against
    // silent no-op installs when a user copy-pastes an rdev-only name.
    let err = parse_chord(&s(&["insert"])).expect_err("insert has no VK mapping");
    assert!(err.contains("insert"), "{err}");
    assert!(err.contains("supported"), "{err}");
}

#[test]
fn accepts_letter_and_digit_triggers_via_ascii_passthrough() {
    // Letters and digits map to their ASCII byte value — the Windows
    // VK table for A..Z and 0..9 is literally the ASCII byte, so a
    // one-character segment is a valid trigger. This is the only path
    // through `vk_from_trigger_name` that returns without a lookup
    // table entry, so it gets its own test. Uses generic `ctrl` /
    // `shift` names because side-specific modifiers are rejected up-
    // front now.
    let a = parse_chord(&s(&["a"])).expect("letter a parses");
    assert_eq!(a.vk, 0x41);
    let z = parse_chord(&s(&["ctrl", "z"])).expect("ctrl+z parses");
    assert_eq!(z.mods, MOD_CONTROL);
    assert_eq!(z.vk, 0x5A);
    let one = parse_chord(&s(&["shift", "1"])).expect("shift+1 parses");
    assert_eq!(one.mods, MOD_SHIFT);
    assert_eq!(one.vk, 0x31);
}

#[test]
fn empty_chord_and_whitespace_segments_report_error() {
    // Empty input is explicitly rejected (upstream `install_hotkey`
    // catches this via `EmptyConfig`, but the driver's parser must
    // agree so a direct caller gets a clear message too).
    let err = parse_chord(&[]).expect_err("empty chord rejected");
    assert!(err.contains("empty"), "{err}");
    // Whitespace-only segments (a user typing `ctrl+  +f9`) are
    // dropped so the config accepts benign spacing without producing a
    // silent parse failure downstream.
    let parsed = parse_chord(&s(&["ctrl", "   ", "f9"])).expect("padded chord parses");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
}

#[test]
fn parse_is_case_insensitive_and_trims() {
    // The tracker's names are lowercase; a config with `CTRL+F9`
    // should install just fine. Trim + ASCII-lowercase before matching.
    let parsed = parse_chord(&s(&["  CTRL  ", "F9"])).expect("case-insensitive parse");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
}

#[test]
fn side_specific_rejection_is_case_insensitive() {
    // A user typing `CTRL_L+F9` should hit the same side-specific
    // rejection as the lowercase form — the guard runs after trim +
    // lowercase, so case must not smuggle a sided alias through.
    let err = parse_chord(&s(&["CTRL_L", "F9"])).expect_err("upper-case CTRL_L is still sided");
    assert!(
        err.contains("side-specific") || err.contains("ctrl_l"),
        "{err}"
    );
}

#[test]
fn vk_helper_returns_none_for_names_the_parser_would_reject() {
    // Belt-and-braces: `vk_from_trigger_name` is exposed pub(crate) for
    // the parser + one telemetry site. Anything the parser rejects
    // must also return None here so the two decisions cannot drift.
    assert!(vk_from_trigger_name("insert").is_none());
    assert!(vk_from_trigger_name("ab").is_none()); // multi-char non-lookup
    assert!(vk_from_trigger_name("!").is_none()); // punctuation
}

#[test]
fn display_string_preserves_input_order_after_trim_and_lowercase() {
    // The `display` field is what the diagnostic log line names —
    // preserving segment order (not `mods|vk` reordering) helps
    // operators grep for exactly the chord they typed.
    let parsed = parse_chord(&s(&["Shift", "CTRL", "f9"])).expect("parses");
    assert_eq!(parsed.display, "shift+ctrl+f9");
}

#[test]
fn pause_key_is_a_supported_trigger() {
    // `pause` is a valid Windows virtual key (VK_PAUSE = 0x13) accepted by
    // RegisterHotKey, even though it is not part of the rdev name table.
    let parsed = parse_chord(&s(&["pause"])).expect("pause parses");
    assert_eq!(parsed.vk, 0x13);
    assert_eq!(parsed.mods, 0);
}

// -----------------------------------------------------------------------
// Loop state-machine tests (LoopState + advance_state).
//
// These pin the driver's press/release lifecycle so a future
// refactor that reintroduced a "first-press-only" bug (the class of
// regression seen on the rdev backend in the rc.10 GUI diagnostic:
// hook installed, first press fires, subsequent presses silently
// swallowed) has to fail a test before landing. The whole point of
// the RegisterHotKey switch is to make repeat cycles trivially work,
// so the tests must actively exercise them.
// -----------------------------------------------------------------------

fn armed_state() -> LoopState {
    let mut s = LoopState::new();
    s.registered = Some(ParsedChord {
        mods: 0,
        vk: 0x78, // f9
        trigger_name: "f9".to_owned(),
        display: "f9".to_owned(),
    });
    s
}

#[test]
fn wm_hotkey_without_registered_chord_emits_nothing() {
    // Belt-and-braces: a stray WM_HOTKEY after Unregister must not
    // fire a press. RegisterHotKey should not deliver WM_HOTKEY for
    // an unregistered id, but the state machine has to be defensive
    // — a race between UnregisterHotKey and an already-queued
    // WM_HOTKEY is theoretically possible.
    let mut s = LoopState::new();
    assert_eq!(
        advance_state(&mut s, LoopStimulus::WmHotkey),
        LoopEmit::None
    );
    assert!(s.pressed_trigger.is_none());
}

#[test]
fn wm_hotkey_when_armed_fires_press_exactly_once() {
    let mut s = armed_state();
    assert_eq!(
        advance_state(&mut s, LoopStimulus::WmHotkey),
        LoopEmit::Press
    );
    // Duplicate WM_HOTKEY (OS repeat leak past MOD_NOREPEAT): must
    // NOT re-fire — the tracker downstream would double-count and
    // the coordinator's press-debounce could still let a spurious
    // start slip through.
    assert_eq!(
        advance_state(&mut s, LoopStimulus::WmHotkey),
        LoopEmit::None
    );
    assert_eq!(s.pressed_trigger, Some(0x78));
}

#[test]
fn poll_up_after_press_fires_release_exactly_once() {
    let mut s = armed_state();
    advance_state(&mut s, LoopStimulus::WmHotkey);
    // Held: no emission.
    assert_eq!(
        advance_state(&mut s, LoopStimulus::PollTriggerDown),
        LoopEmit::None
    );
    // Released: fires release.
    assert_eq!(
        advance_state(&mut s, LoopStimulus::PollTriggerUp),
        LoopEmit::Release
    );
    assert!(s.pressed_trigger.is_none());
    // Second poll-up after release: nothing to release.
    assert_eq!(
        advance_state(&mut s, LoopStimulus::PollTriggerUp),
        LoopEmit::None
    );
}

#[test]
fn multiple_consecutive_press_release_cycles_all_fire() {
    // THE regression. rc.10 GUI diagnostic showed rdev's LL hook
    // going deaf after the first callback (state stuck / hook torn
    // down); the whole point of the RegisterHotKey switch is that
    // WM_HOTKEY is delivered ONCE per physical press through USER32,
    // so N presses fire N times. Assert exactly that with a five-cycle
    // burst — long enough that a "first-N-only" regression (N in {1,
    // 2, 3, 4}) fails the test.
    let mut s = armed_state();
    for cycle in 0..5 {
        let press = advance_state(&mut s, LoopStimulus::WmHotkey);
        assert_eq!(
            press,
            LoopEmit::Press,
            "cycle {cycle}: WM_HOTKEY must emit press"
        );
        // Some polls while held.
        for _ in 0..3 {
            assert_eq!(
                advance_state(&mut s, LoopStimulus::PollTriggerDown),
                LoopEmit::None,
                "cycle {cycle}: held-key polls emit nothing"
            );
        }
        // Release.
        let release = advance_state(&mut s, LoopStimulus::PollTriggerUp);
        assert_eq!(
            release,
            LoopEmit::Release,
            "cycle {cycle}: trigger-up must emit release"
        );
        assert!(
            s.pressed_trigger.is_none(),
            "cycle {cycle}: pressed_trigger must clear on release"
        );
    }
}

#[test]
fn rapid_burst_press_release_press_release_stays_in_sync() {
    // The tight-cycle variant of the multi-press test: no held-key
    // polls between press and release. Guards against a state machine
    // that only clears `pressed_trigger` on the transition through a
    // "still-held" poll.
    let mut s = armed_state();
    for _ in 0..10 {
        assert_eq!(
            advance_state(&mut s, LoopStimulus::WmHotkey),
            LoopEmit::Press
        );
        assert_eq!(
            advance_state(&mut s, LoopStimulus::PollTriggerUp),
            LoopEmit::Release
        );
    }
}

#[test]
fn poll_stimuli_before_any_press_are_ignored() {
    // The loop only polls when `pressed_trigger` is set, but the
    // helper must also be safe to call in the "spurious poll" case
    // (e.g. a future refactor that unconditionally polls). No press
    // ever fired, so no release should either.
    let mut s = armed_state();
    assert_eq!(
        advance_state(&mut s, LoopStimulus::PollTriggerUp),
        LoopEmit::None
    );
    assert_eq!(
        advance_state(&mut s, LoopStimulus::PollTriggerDown),
        LoopEmit::None
    );
    assert!(s.pressed_trigger.is_none());
}

// -----------------------------------------------------------------------
// Modifier VK groups (#650 — release chord when modifier
// released mid-hold).
//
// The release-polling path in `run_msg_loop` treats the chord as
// released as soon as ANY required modifier family stops registering as
// down — so `ctrl+f9` fires ChordRelease when the user lets go of Ctrl
// while still holding F9. The pure `required_modifier_vk_groups` helper
// selects the VKs to poll; test it directly since GetAsyncKeyState is
// not driveable from a unit test.
// -----------------------------------------------------------------------

// Windows VK constants (same as the production module's private consts).
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

#[test]
fn required_modifier_vk_groups_is_empty_for_no_mods() {
    // A chord with no modifiers (e.g. bare `f9`) has nothing to poll:
    // the trigger VK is the sole liveness signal.
    assert!(required_modifier_vk_groups(0).is_empty());
}

#[test]
fn required_modifier_vk_groups_maps_each_family_to_its_vk() {
    // Each MOD_* bit maps to the generic VK for its family. Control /
    // Shift / Alt each have a single-VK group; Win has BOTH LWIN and
    // RWIN because Windows has no unified Win VK.
    assert_eq!(
        required_modifier_vk_groups(MOD_CONTROL),
        vec![vec![VK_CONTROL]]
    );
    assert_eq!(required_modifier_vk_groups(MOD_SHIFT), vec![vec![VK_SHIFT]]);
    assert_eq!(required_modifier_vk_groups(MOD_ALT), vec![vec![VK_MENU]]);
    assert_eq!(
        required_modifier_vk_groups(MOD_WIN),
        vec![vec![VK_LWIN, VK_RWIN]]
    );
}

// -----------------------------------------------------------------------
// plan_register — the "validate BEFORE unregister" contract (
// Verify modifier release handling.
//
// The message loop's Register handler now calls `plan_register` before
// touching any OS state. A parse failure must be surfaced as
// `RegisterPlan::Reject` so `handle_command` acks the error WITHOUT
// unregistering the previously-working binding — the guarantee that
// keeps the process from ending up with no listener at all when the
// supervisor sends a bad new chord (e.g. the user typed a side-specific
// alias into settings and asked for a resume-with-new-chord).
// -----------------------------------------------------------------------

#[test]
fn plan_register_accepts_valid_chord() {
    let plan = plan_register(&s(&["ctrl", "f9"]));
    match plan {
        RegisterPlan::Install(chord) => {
            assert_eq!(chord.mods, MOD_CONTROL);
            assert_eq!(chord.vk, 0x78);
        }
        RegisterPlan::Reject(msg) => panic!("expected Install, got Reject({msg:?})"),
    }
}

#[test]
fn plan_register_rejects_side_specific_before_state_touch() {
    // The P1 fix's core invariant: a side-specific chord (which the
    // parser now rejects) must produce Reject so `handle_command`
    // returns without calling `unregister_current`. If a future
    // refactor moved the OS unregister ahead of the plan gate, this
    // test would still pass — the ordering discipline is enforced by
    // the test below (`register_reject_leaves_previous_binding_intact`).
    let plan = plan_register(&s(&["ctrl_l", "f9"]));
    assert!(
        matches!(plan, RegisterPlan::Reject(_)),
        "side-specific chord must plan to Reject: {plan:?}"
    );
}

#[test]
fn plan_register_rejects_modifier_only_chord() {
    let plan = plan_register(&s(&["ctrl"]));
    assert!(
        matches!(plan, RegisterPlan::Reject(_)),
        "modifier-only chord must plan to Reject: {plan:?}"
    );
}

#[test]
fn plan_register_rejects_empty_chord() {
    let plan = plan_register(&[]);
    assert!(
        matches!(plan, RegisterPlan::Reject(_)),
        "empty chord must plan to Reject: {plan:?}"
    );
}

#[test]
fn required_modifier_vk_groups_composes_multiple_families() {
    // Ctrl+Shift+F9 style chord: both families must poll their VKs.
    // Order mirrors the bit-check order in the helper (Control, Shift,
    // Alt, Win) so downstream consumers can rely on a stable
    // enumeration for logging.
    assert_eq!(
        required_modifier_vk_groups(MOD_CONTROL | MOD_SHIFT),
        vec![vec![VK_CONTROL], vec![VK_SHIFT]]
    );
    assert_eq!(
        required_modifier_vk_groups(MOD_CONTROL | MOD_SHIFT | MOD_ALT | MOD_WIN),
        vec![
            vec![VK_CONTROL],
            vec![VK_SHIFT],
            vec![VK_MENU],
            vec![VK_LWIN, VK_RWIN],
        ]
    );
}

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
