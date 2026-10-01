use super::*;

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
