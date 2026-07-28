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
    advance_state, parse_chord, vk_from_trigger_name, LoopEmit, LoopState, LoopStimulus,
    ParsedChord, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
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
fn parses_ctrl_l_plus_function_key() {
    // The most common Windows-friendly chord: one modifier + one trigger.
    let parsed = parse_chord(&s(&["ctrl_l", "f9"])).expect("ctrl_l+f9 parses");
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
fn alt_gr_and_right_alt_and_ralt_all_map_to_mod_alt() {
    // Aliases the rdev driver accepts (P2 #346 finding 4) must map to
    // the same MOD_ALT flag here so users who typed any of the three
    // forms into their config get an equivalent install.
    for alt_alias in ["alt_gr", "right_alt", "ralt", "alt_r", "alt_l"] {
        let parsed = parse_chord(&s(&[alt_alias, "f9"]))
            .unwrap_or_else(|e| panic!("alt alias {alt_alias:?} must parse: {e}"));
        assert_eq!(parsed.mods, MOD_ALT, "alias {alt_alias:?} → MOD_ALT");
    }
}

#[test]
fn win_and_cmd_family_names_map_to_mod_win() {
    // The tracker names the Windows key as `cmd_l` / `cmd_r` (macOS
    // vocabulary — inherited from pynput); the RegisterHotKey flag is
    // MOD_WIN. Both `cmd*` and the friendlier `win*` names accepted.
    for win_name in ["cmd", "cmd_l", "cmd_r", "win", "win_l", "win_r"] {
        let parsed = parse_chord(&s(&[win_name, "f9"]))
            .unwrap_or_else(|e| panic!("win alias {win_name:?} must parse: {e}"));
        assert_eq!(parsed.mods, MOD_WIN);
    }
}

#[test]
fn rejects_modifier_only_chord_with_actionable_message() {
    // The signature limitation of RegisterHotKey: modifier-only chords
    // are NOT supported. The error message must name the constraint
    // AND point the user at the escape hatch (VOICEPI_HOTKEY_DRIVER=rdev)
    // so a user seeing this in the diagnostic log can fix it without
    // reading source.
    let err = parse_chord(&s(&["ctrl_l"])).expect_err("bare modifier must be rejected");
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
    let err = parse_chord(&s(&["ctrl_l", "shift_r"])).expect_err("bare modifiers must be rejected");
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
    // table entry, so it gets its own test.
    let a = parse_chord(&s(&["a"])).expect("letter a parses");
    assert_eq!(a.vk, 0x41);
    let z = parse_chord(&s(&["ctrl_l", "z"])).expect("ctrl_l+z parses");
    assert_eq!(z.mods, MOD_CONTROL);
    assert_eq!(z.vk, 0x5A);
    let one = parse_chord(&s(&["shift_l", "1"])).expect("shift_l+1 parses");
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
    // The tracker's names are lowercase; a config with `CTRL_L+F9`
    // should install just fine. Trim + ASCII-lowercase before matching.
    let parsed = parse_chord(&s(&["  CTRL_L  ", "F9"])).expect("case-insensitive parse");
    assert_eq!(parsed.mods, MOD_CONTROL);
    assert_eq!(parsed.vk, 0x78);
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
    let parsed = parse_chord(&s(&["Shift_L", "CTRL", "f9"])).expect("parses");
    assert_eq!(parsed.display, "shift_l+ctrl+f9");
}

#[test]
fn pause_key_is_a_supported_trigger() {
    // Follow-up from the rc.10 user report: `pause` is NOT in the rdev
    // driver's supported-name table (RDEV_SUPPORTED_NAMES has no
    // `pause`), but it IS a valid Windows virtual key (VK_PAUSE = 0x13)
    // that RegisterHotKey accepts. The register driver must accept it
    // so users with a `pause` chord can install successfully on
    // Windows without changing their config.
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
    // THE regression pin. rc.10 GUI diagnostic showed rdev's LL hook
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
