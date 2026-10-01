//! Coverage for the session cancellation boundary.

use super::tests_support::*;
use super::SessionState;
use crate::dictate::feedback::CueKind;

#[test]
fn stale_cancel_while_idle_is_a_noop() {
    // A cancel that arrives when the session is idle (no recording in
    // flight) must do nothing, emit nothing — Python's
    // `if not self.recording: return`.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.cancel(0, &mut buf).expect("cancel-while-idle");
    s.cancel(42, &mut buf)
        .expect("cancel-while-idle (bogus epoch)");
    assert!(buf.is_empty(), "cancel-while-idle must not emit any events");
    assert_eq!(s.state(), SessionState::Idle);
    assert_eq!(s.epoch(), 0);
}

#[test]
fn chord_cancel_plays_the_stop_cue() {
    // Parity with `vp_dictate.py::_cancel_and_discard` (lines 662-681):
    // Python routes the cancel through `_stop_and_transcribe`, which
    // fires `play_cue("stop")` even on the discard branch. The Rust
    // cancel() shortcuts around stop_and_transcribe, so the cue has
    // to be fired explicitly here or a chord-cancel would silently
    // drop the audible "recording ended" signal.
    let transcribe = TestTranscribe::returning_text("hi");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let (sink, log) = RecordingCueSink::new();
    s = s.with_cue_sink(Box::new(sink));

    let epoch = s.start(&mut buf).expect("start");
    s.cancel(epoch, &mut buf).expect("cancel");
    assert_eq!(
        *log.lock().unwrap(),
        vec![CueKind::Start, CueKind::Stop],
        "chord-cancel must fire Start then Stop just like a full utterance"
    );
    assert_eq!(s.state(), SessionState::Idle);
}

#[test]
fn stale_cancel_does_not_play_a_stop_cue() {
    // A stale cancel (epoch mismatch) must NOT fire a second Stop cue,
    // because doing so would emit an audible "recording ended" while
    // the NEW recording is still capturing frames. Only the Start cue
    // from the fresh `start()` should be present.
    let transcribe = TestTranscribe::returning_text("hi");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let (sink, log) = RecordingCueSink::new();
    s = s.with_cue_sink(Box::new(sink));

    let epoch = s.start(&mut buf).expect("start");
    let stale_epoch = epoch.wrapping_sub(1);
    s.cancel(stale_epoch, &mut buf).expect("cancel");
    assert_eq!(
        *log.lock().unwrap(),
        vec![CueKind::Start],
        "stale cancel must not fire a Stop cue"
    );
    assert!(matches!(s.state(), SessionState::Recording { .. }));
}

#[test]
fn cancel_calls_ducker_exit() {
    // Parity with the Python chord-cancel path: `_cancel_and_discard`
    // routes through `_stop_and_transcribe`, whose `finally` restores
    // the ducker. The Rust `cancel()` shortcuts around
    // `stop_and_transcribe`, so it MUST call `exit()` explicitly or a
    // chord-cancel would leave background media dampened forever.
    let transcribe = TestTranscribe::returning_text("hi");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let (ducker, events) = RecordingDucker::new();
    s = s.with_ducker(Box::new(ducker));

    let epoch = s.start(&mut buf).expect("start");
    s.cancel(epoch, &mut buf).expect("cancel");

    assert_eq!(
        *events.lock().unwrap(),
        vec!["enter", "exit"],
        "chord-cancel must call exit() just like a full utterance"
    );
}
