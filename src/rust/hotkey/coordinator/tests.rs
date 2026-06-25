use super::*;

fn hold_options() -> Options {
    Options {
        mode: Mode::HoldToTalk,
    }
}

fn toggle_options() -> Options {
    Options { mode: Mode::Toggle }
}

#[test]
fn idle_then_press_enters_recording_and_emits_start() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    let action = step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    assert_eq!(action, Some(CoordinatorAction::StartRecording(1)));
    assert_eq!(s.stage, Stage::Recording(1));
    assert_eq!(s.next_id, 1);
}

#[test]
fn recording_then_release_enters_processing_and_emits_stop() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    let action = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(500),
        CoordinatorEvent::Release,
    );
    assert_eq!(action, Some(CoordinatorAction::StopAndTranscribe(1)));
    assert_eq!(s.stage, Stage::Processing(1));
}

#[test]
fn processing_then_finished_returns_to_idle() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(500),
        CoordinatorEvent::Release,
    );
    let action = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(900),
        CoordinatorEvent::ProcessingFinished(1),
    );
    assert_eq!(action, None);
    assert_eq!(s.stage, Stage::Idle);
    assert_eq!(
        s.last_idle_press, None,
        "debounce re-armed after Processing→Idle"
    );
}

#[test]
fn second_press_within_debounce_window_is_dropped() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    let first = step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    assert!(matches!(first, Some(CoordinatorAction::StartRecording(_))));
    // Manually drop back to Idle WITHOUT clearing last_idle_press — the
    // debounce is meant to catch presses on the SAME Idle state, so
    // simulate the chord breaking before the host had time to react.
    s.stage = Stage::Idle;
    // Second press only 10 ms after the first → suppressed.
    let action = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(10),
        CoordinatorEvent::Press,
    );
    assert_eq!(action, None);
    assert_eq!(s.stage, Stage::Idle);
    // Third press well outside the debounce window → fires.
    let action = step(
        &mut s,
        hold_options(),
        t0 + PRESS_DEBOUNCE + Duration::from_millis(5),
        CoordinatorEvent::Press,
    );
    assert!(matches!(action, Some(CoordinatorAction::StartRecording(_))));
}

#[test]
fn spurious_release_in_idle_is_dropped() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    let action = step(&mut s, hold_options(), t0, CoordinatorEvent::Release);
    assert_eq!(action, None);
    assert_eq!(s.stage, Stage::Idle);
}

#[test]
fn spurious_release_in_processing_is_dropped() {
    // The #254-class hole: release races processing-finished and tries
    // to start a fresh recording. The drop-guard makes it a no-op.
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(300),
        CoordinatorEvent::Release,
    );
    assert_eq!(s.stage, Stage::Processing(1));
    let action = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(310),
        CoordinatorEvent::Release,
    );
    assert_eq!(action, None);
    assert_eq!(
        s.stage,
        Stage::Processing(1),
        "still Processing after spurious release"
    );
}

#[test]
fn press_during_recording_is_ignored_keyrepeat() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    // Key-repeat ~50 ms later: must not re-fire StartRecording.
    let action = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(50),
        CoordinatorEvent::Press,
    );
    assert_eq!(action, None);
    assert_eq!(s.stage, Stage::Recording(1));
}

#[test]
fn cancel_in_recording_emits_cancel_and_returns_to_idle() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    let action = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(200),
        CoordinatorEvent::Cancel,
    );
    assert_eq!(action, Some(CoordinatorAction::CancelRecording(1)));
    assert_eq!(s.stage, Stage::Idle);
}

#[test]
fn cancel_in_idle_is_noop() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    let action = step(&mut s, hold_options(), t0, CoordinatorEvent::Cancel);
    assert_eq!(action, None);
    assert_eq!(s.stage, Stage::Idle);
}

#[test]
fn recording_id_increments_per_cycle() {
    let mut s = StepState::new();
    let mut t = Instant::now();
    for expected in 1..=3 {
        let a = step(&mut s, hold_options(), t, CoordinatorEvent::Press);
        assert_eq!(a, Some(CoordinatorAction::StartRecording(expected)));
        t += Duration::from_millis(100);
        let a = step(&mut s, hold_options(), t, CoordinatorEvent::Release);
        assert_eq!(a, Some(CoordinatorAction::StopAndTranscribe(expected)));
        t += Duration::from_millis(100);
        step(
            &mut s,
            hold_options(),
            t,
            CoordinatorEvent::ProcessingFinished(expected),
        );
        t += PRESS_DEBOUNCE + Duration::from_millis(5);
    }
}

#[test]
fn spawn_thread_round_trip() {
    // Light end-to-end test of the spawn() wrapper: press → release →
    // processing-finished delivered via the mpsc channel produces the
    // expected actions in order on the action sink.
    let (action_tx, action_rx) = mpsc::channel();
    let (handle, thread) = spawn(
        hold_options(),
        move |action| {
            action_tx.send(action).unwrap();
        },
        Instant::now,
    );
    handle.send(CoordinatorEvent::Press);
    handle.send(CoordinatorEvent::Release);
    handle.send(CoordinatorEvent::ProcessingFinished(1));

    let first = action_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first action");
    assert!(matches!(first, CoordinatorAction::StartRecording(_)));
    let second = action_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("second action");
    assert!(matches!(second, CoordinatorAction::StopAndTranscribe(_)));
    // ProcessingFinished does not emit an action.
    assert!(action_rx.recv_timeout(Duration::from_millis(100)).is_err());

    handle.shutdown();
    thread.join();
}

// -----------------------------------------------------------------------
// Toggle mode (P2 #4).
// -----------------------------------------------------------------------

#[test]
fn toggle_mode_release_does_not_stop_recording() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    let start = step(&mut s, toggle_options(), t0, CoordinatorEvent::Press);
    assert_eq!(start, Some(CoordinatorAction::StartRecording(1)));
    // Release in toggle mode is a no-op while Recording.
    let no_stop = step(
        &mut s,
        toggle_options(),
        t0 + Duration::from_millis(50),
        CoordinatorEvent::Release,
    );
    assert_eq!(no_stop, None);
    assert_eq!(s.stage, Stage::Recording(1));
}

#[test]
fn toggle_mode_second_press_stops_recording() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, toggle_options(), t0, CoordinatorEvent::Press);
    // User let go of the chord; no transition.
    step(
        &mut s,
        toggle_options(),
        t0 + Duration::from_millis(50),
        CoordinatorEvent::Release,
    );
    // Second press → stop and transcribe.
    let stop = step(
        &mut s,
        toggle_options(),
        t0 + Duration::from_millis(2000),
        CoordinatorEvent::Press,
    );
    assert_eq!(stop, Some(CoordinatorAction::StopAndTranscribe(1)));
    assert_eq!(s.stage, Stage::Processing(1));
}

#[test]
fn toggle_mode_cancel_still_works() {
    // Cancel semantics should be identical in either mode — a foreign key
    // mid-recording always cancels.
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, toggle_options(), t0, CoordinatorEvent::Press);
    let cancel = step(
        &mut s,
        toggle_options(),
        t0 + Duration::from_millis(100),
        CoordinatorEvent::Cancel,
    );
    assert_eq!(cancel, Some(CoordinatorAction::CancelRecording(1)));
    assert_eq!(s.stage, Stage::Idle);
}

// -----------------------------------------------------------------------
// Held press across Processing (P2 #8).
// -----------------------------------------------------------------------

#[test]
fn press_held_through_processing_starts_new_recording_on_completion() {
    // Cycle 1: press, release, processing starts. User keeps PTT held →
    // tracker emits another ChordPress while we're in Processing. When the
    // host signals ProcessingFinished, we should immediately start a new
    // recording rather than wait for release-then-press.
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(300),
        CoordinatorEvent::Release,
    );
    assert_eq!(s.stage, Stage::Processing(1));
    // Held press lands during Processing — latched, no action.
    let latched = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(500),
        CoordinatorEvent::Press,
    );
    assert_eq!(latched, None);
    assert!(s.pending_press);
    // Processing completes — pending press fires immediately as Start.
    let restart = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(800),
        CoordinatorEvent::ProcessingFinished(1),
    );
    assert_eq!(restart, Some(CoordinatorAction::StartRecording(2)));
    assert_eq!(s.stage, Stage::Recording(2));
    assert!(!s.pending_press);
}

#[test]
fn release_during_processing_clears_pending_press() {
    // Variation of the above: user pressed during Processing then let go
    // before transcription finished. The Press latches but the Release
    // clears it — no auto-start when Processing completes.
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(300),
        CoordinatorEvent::Release,
    );
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(500),
        CoordinatorEvent::Press,
    );
    assert!(s.pending_press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(600),
        CoordinatorEvent::Release,
    );
    assert!(!s.pending_press);
    let nothing = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(800),
        CoordinatorEvent::ProcessingFinished(1),
    );
    assert_eq!(nothing, None);
    assert_eq!(s.stage, Stage::Idle);
}

// -----------------------------------------------------------------------
// Stale ProcessingFinished id (P2 #9).
// -----------------------------------------------------------------------

#[test]
fn stale_processing_finished_during_new_recording_is_ignored() {
    // Cycle 1: press → release → Processing(1). The host's
    // ProcessingFinished(1) gets queued behind a new manual Cancel that
    // returns to Idle, plus a fresh Press → Recording(2). Now the stale
    // ProcessingFinished(1) lands while we're in Recording(2). It must
    // NOT clear the live recording.
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(100),
        CoordinatorEvent::Release,
    );
    assert_eq!(s.stage, Stage::Processing(1));
    // Pretend the host crashed/restarted and we manually reset (mirrors
    // what would happen if a real cancel + restart raced the completion).
    s.stage = Stage::Idle;
    s.last_idle_press = None;
    let restart = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_secs(2),
        CoordinatorEvent::Press,
    );
    assert_eq!(restart, Some(CoordinatorAction::StartRecording(2)));
    assert_eq!(s.stage, Stage::Recording(2));
    // Stale completion for cycle 1 arrives — must be ignored.
    let stale = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_secs(2) + Duration::from_millis(10),
        CoordinatorEvent::ProcessingFinished(1),
    );
    assert_eq!(stale, None);
    assert_eq!(
        s.stage,
        Stage::Recording(2),
        "live recording preserved across stale completion",
    );
}

#[test]
fn matching_processing_finished_during_processing_completes_cycle() {
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(100),
        CoordinatorEvent::Release,
    );
    assert_eq!(s.stage, Stage::Processing(1));
    let done = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(200),
        CoordinatorEvent::ProcessingFinished(1),
    );
    assert_eq!(done, None);
    assert_eq!(s.stage, Stage::Idle);
}

#[test]
fn mismatched_processing_finished_in_processing_is_dropped() {
    // Host sent ProcessingFinished(99) by mistake (or after a crash
    // recovery). Live state is Processing(1) — the mismatched id must NOT
    // wake us up to Idle, because there is no real evidence that the cycle
    // really finished.
    let mut s = StepState::new();
    let t0 = Instant::now();
    step(&mut s, hold_options(), t0, CoordinatorEvent::Press);
    step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(100),
        CoordinatorEvent::Release,
    );
    assert_eq!(s.stage, Stage::Processing(1));
    let ignored = step(
        &mut s,
        hold_options(),
        t0 + Duration::from_millis(200),
        CoordinatorEvent::ProcessingFinished(99),
    );
    assert_eq!(ignored, None);
    assert_eq!(s.stage, Stage::Processing(1));
}
