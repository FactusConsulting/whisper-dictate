use super::*;

#[test]
fn decide_terminal_no_early_exit_when_flag_off() {
    let counters = Counters::default();
    let start = Instant::now();
    let action =
        CoordinatorAction::StartRecording(crate::hotkey::coordinator::RecordingId::from(1u8));
    assert!(decide_terminal(&action, false, &counters, start).is_none());
}

#[test]
fn decide_terminal_fires_on_start_recording_when_flag_on() {
    let counters = Counters::default();
    counters.events.store(4, Ordering::Relaxed);
    counters.chords.store(1, Ordering::Relaxed);
    let start = Instant::now();
    let action =
        CoordinatorAction::StartRecording(crate::hotkey::coordinator::RecordingId::from(1u8));
    let term = decide_terminal(&action, true, &counters, start).expect("terminal");
    match term {
        CaptureEvent::ExitOnChord { events, chords, .. } => {
            assert_eq!(events, 4);
            assert_eq!(chords, 1);
        }
        other => panic!("expected ExitOnChord, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// validate_driver_flag — CLI-level driver selection (audit item 5 prereq 2)
// -----------------------------------------------------------------------

fn rec_id(n: u8) -> crate::hotkey::coordinator::RecordingId {
    crate::hotkey::coordinator::RecordingId::from(n)
}

#[test]
fn counts_as_chord_true_only_for_start_recording() {
    assert!(counts_as_chord(&CoordinatorAction::StartRecording(rec_id(
        1
    ))));
    assert!(!counts_as_chord(&CoordinatorAction::StopAndTranscribe(
        rec_id(1)
    )));
    assert!(!counts_as_chord(&CoordinatorAction::CancelRecording(
        rec_id(1)
    )));
}

#[test]
fn action_sink_stamps_each_chord_with_its_event_source_focus() {
    let counters = Arc::new(Counters::default());
    let (tx, rx) = mpsc::channel();
    let mut sink = build_capture_action_sink(
        counters,
        tx,
        Arc::new(OnceLock::new()),
        Instant::now(),
        false,
    );

    sink(CoordinatorAction::StartRecording(rec_id(1)), Some(false));
    sink(CoordinatorAction::StartRecording(rec_id(2)), Some(true));

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(matches!(
        events.as_slice(),
        [
            CaptureEvent::ChordMatched {
                focused: Some(false),
                ..
            },
            CaptureEvent::ChordMatched {
                focused: Some(true),
                ..
            }
        ]
    ));
}

#[test]
fn one_full_chord_cycle_increments_counter_once() {
    // Simulate what the action sink actually does: increment on every
    // action for which `counts_as_chord` is true. Two full chord cycles
    // (press → release, press → cancel) must report chords = 2, not 4.
    let counters = Counters::default();
    let cycle = [
        CoordinatorAction::StartRecording(rec_id(1)),
        CoordinatorAction::StopAndTranscribe(rec_id(1)),
        CoordinatorAction::StartRecording(rec_id(2)),
        CoordinatorAction::CancelRecording(rec_id(2)),
    ];
    for action in &cycle {
        if counts_as_chord(action) {
            counters.chords.fetch_add(1, Ordering::Relaxed);
        }
    }
    assert_eq!(counters.chords.load(Ordering::Relaxed), 2);
}

#[test]
fn decide_terminal_ignores_release_and_cancel_actions() {
    // Only the *matched* rising edge triggers exit-on-chord — a release
    // or cancel arriving before a start would exit prematurely.
    let counters = Counters::default();
    let start = Instant::now();
    let release =
        CoordinatorAction::StopAndTranscribe(crate::hotkey::coordinator::RecordingId::from(1u8));
    let cancel =
        CoordinatorAction::CancelRecording(crate::hotkey::coordinator::RecordingId::from(1u8));
    assert!(decide_terminal(&release, true, &counters, start).is_none());
    assert!(decide_terminal(&cancel, true, &counters, start).is_none());
}

// -----------------------------------------------------------------------
// Counter plumbing (build_raw_tap + action sink)
//
// The pre-existing tests here only exercised the FORMATTERS with
// hand-supplied numbers, so `foreign_keys` could be -- and was -- never
// incremented anywhere while every test stayed green. These drive the
// producing side instead.
// -----------------------------------------------------------------------

#[cfg(feature = "rust-hotkeys")]
mod processing_stage {
    use super::super::*;
    use crate::hotkey::coordinator::{self, CoordinatorEvent, Mode, Options};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Drive a real coordinator through TWO complete press -> release
    /// cycles, wiring the sink exactly the way `run_capture` does.
    ///
    /// Before the fix this yielded a single StartRecording: the first
    /// release parked the coordinator in `Stage::Processing` and nothing
    /// ever completed it, so every later press was swallowed. Observed on
    /// a real Wayland box as 14 keypresses producing no chord at all.
    ///
    /// The second cycle is sent only AFTER the first stop has been
    /// observed, mirroring reality -- a human cannot press again before
    /// the release they just made has been processed. Queueing all four
    /// events up front instead would land the second press inside
    /// `Processing`, where the coordinator legitimately defers it, and
    /// the test would be measuring event ordering rather than the missing
    /// feedback.
    fn start_recordings_over_two_cycles(complete_stage: bool) -> usize {
        let (action_tx, action_rx) = mpsc::channel();
        let slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
        let sink_slot = Arc::clone(&slot);
        let sink = move |action: CoordinatorAction| {
            if let CoordinatorAction::StopAndTranscribe(id) = action {
                if complete_stage {
                    complete_processing_stage(sink_slot.get(), id);
                }
            }
            let _ = action_tx.send(action);
        };
        // Inject a clock that jumps well past PRESS_DEBOUNCE (30 ms)
        // between events. Without it the second press is debounced away
        // and the test would "pass" for entirely the wrong reason -- it
        // would measure the debounce, not the Processing stage.
        let base = Instant::now();
        let mut ticks = 0u32;
        let clock = move || {
            ticks += 1;
            base + Duration::from_millis(100 * u64::from(ticks))
        };
        let (handle, thread) = coordinator::spawn(
            Options {
                mode: Mode::HoldToTalk,
                // This suite exercises the SINK-side completion path
                // explicitly; auto-complete would defeat the point.
                auto_complete_processing: false,
            },
            sink,
            clock,
        );
        let _ = slot.set(handle.clone());

        let mut starts = 0usize;
        let recv =
            |rx: &mpsc::Receiver<CoordinatorAction>| rx.recv_timeout(Duration::from_millis(500));

        handle.send(CoordinatorEvent::Press);
        handle.send(CoordinatorEvent::Release);
        // Cycle 1: expect StartRecording then StopAndTranscribe.
        if matches!(recv(&action_rx), Ok(CoordinatorAction::StartRecording(_))) {
            starts += 1;
        }
        let _ = recv(&action_rx);
        // Let the ProcessingFinished the sink just queued be consumed
        // before the next press -- this is the human gap.
        std::thread::sleep(Duration::from_millis(50));

        handle.send(CoordinatorEvent::Press);
        handle.send(CoordinatorEvent::Release);
        if matches!(recv(&action_rx), Ok(CoordinatorAction::StartRecording(_))) {
            starts += 1;
        }

        handle.shutdown();
        drop(thread);
        starts
    }

    #[test]
    fn second_chord_fires_when_the_processing_stage_is_completed() {
        assert_eq!(
            start_recordings_over_two_cycles(true),
            2,
            "both chords must fire once the sink completes the Processing stage"
        );
    }

    #[test]
    fn without_completion_the_coordinator_goes_deaf_after_one_chord() {
        // Pins the mechanism, so a future change that drops the
        // ProcessingFinished feedback fails loudly here instead of
        // silently making the diagnostic lie again.
        assert_eq!(
            start_recordings_over_two_cycles(false),
            1,
            "without ProcessingFinished the coordinator stays in Processing"
        );
    }

    /// Queue two press/release pairs without allowing the sink to run
    /// between them. Inline completion should leave the second pair
    /// ready to start instead of leaving the coordinator in Processing.
    fn starts_when_second_pair_is_queued_up_front(auto_complete: bool) -> usize {
        let (action_tx, action_rx) = mpsc::channel();
        let sink = move |action: CoordinatorAction| {
            let _ = action_tx.send(action);
        };
        let base = Instant::now();
        let mut ticks = 0u32;
        let clock = move || {
            ticks += 1;
            base + Duration::from_millis(100 * u64::from(ticks))
        };
        let (handle, thread) = coordinator::spawn(
            Options {
                mode: Mode::HoldToTalk,
                auto_complete_processing: auto_complete,
            },
            sink,
            clock,
        );

        // Queue both cycles up front so the completion ordering is tested.
        handle.send(CoordinatorEvent::Press);
        handle.send(CoordinatorEvent::Release);
        handle.send(CoordinatorEvent::Press);
        handle.send(CoordinatorEvent::Release);

        let mut starts = 0usize;
        while let Ok(action) = action_rx.recv_timeout(Duration::from_millis(500)) {
            if matches!(action, CoordinatorAction::StartRecording(_)) {
                starts += 1;
            }
            if starts == 2 {
                break;
            }
        }

        handle.shutdown();
        drop(thread);
        starts
    }

    #[test]
    fn auto_complete_lets_the_second_of_two_queued_pairs_fire() {
        assert_eq!(
            starts_when_second_pair_is_queued_up_front(true),
            2,
            "with auto_complete_processing the coordinator must reach \
             Idle before the queued second Press is consumed",
        );
    }

    #[test]
    fn without_auto_complete_the_second_queued_pair_is_swallowed() {
        // Without inline completion, the coordinator remains in its
        // processing stage after the first release.
        assert_eq!(
            starts_when_second_pair_is_queued_up_front(false),
            1,
            "the second pair cannot start while processing remains pending",
        );
    }
}
