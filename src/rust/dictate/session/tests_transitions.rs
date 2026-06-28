//! Supplementary state-transition invariants for [`super::DictateSession`].
//!
//! These tests pin the small-but-load-bearing properties that the six
//! ported `tests_ported.rs` tests don't directly cover — they're the
//! kind of invariant a refactor can quietly break, so we keep them
//! explicit:
//!
//! - frames pushed outside Recording are dropped;
//! - `start()` while active is an error (epoch NOT bumped, state
//!   unchanged);
//! - `stop_and_transcribe()` while idle is a silent no-op;
//! - transcribe-error → no_text/no_speech;
//! - empty transcribe result → no_text/empty;
//! - the opening → recording transition shape is exact;
//! - cancel-while-idle is a silent no-op;
//! - inject failure still emits the utterance event (Python parity);
//! - epochs increase monotonically across start() calls.

use super::tests_support::*;
use super::{SessionError, SessionState, UtteranceOutcome};

#[test]
fn push_frame_while_idle_is_dropped() {
    // Frames pushed outside Recording are discarded — matches the
    // Python capture mixin's `if self.recording` ingestion gate.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    s.push_frame(&one_second_pcm());
    // Now actually run a recording with no real frames pushed — the
    // session must see an empty buffer (NoAudio outcome).
    s.start(&mut buf).expect("start");
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert_eq!(outcome, UtteranceOutcome::NoAudio);
}

#[test]
fn start_while_active_is_an_error() {
    // Python's `_start` early-returns silently; the Rust port returns
    // `AlreadyActive` so a buggy caller can't accidentally skip a
    // recording. The state must NOT change and the epoch must NOT bump.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    let first = s.start(&mut buf).expect("start#1");
    let err = s.start(&mut buf).expect_err("nested start must error");
    assert!(
        matches!(err, SessionError::AlreadyActive { .. }),
        "expected AlreadyActive, got {err:?}"
    );
    assert_eq!(s.epoch(), first, "epoch must NOT bump on a refused start");
    assert!(matches!(s.state(), SessionState::Recording { .. }));
}

#[test]
fn stop_while_idle_is_a_noop() {
    // `_stop_and_transcribe` early-returns on `not self.recording` in
    // Python; the Rust port surfaces that as `NotRecording` with no
    // events emitted.
    let transcribe = TestTranscribe::returning_text("never");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert_eq!(outcome, UtteranceOutcome::NotRecording);
    assert!(buf.is_empty(), "stop-when-idle must not emit any events");
}

#[test]
fn transcribe_error_emits_no_text_no_speech() {
    // Python `_transcribe_pcm` wraps any model exception and surfaces
    // it as `reason="no_speech"` on the no-text event.
    let transcribe = TestTranscribe::returning_error("model panicked");
    let inject = TestInject::new();
    let (s, _) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(
        outcome,
        UtteranceOutcome::NoText {
            reason: "no_speech"
        }
    ));
    assert!(s.inject_backend().injected.borrow().is_empty());
    let no_text: Vec<_> = parse_events(&bytes)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1);
    assert_eq!(no_text[0]["reason"], "no_speech");
}

#[test]
fn empty_transcribe_result_emits_no_text_empty() {
    // Mirrors the Python branch that maps an empty `result.text` to
    // `reason="empty"` (the `is_hallucination` flag is irrelevant on an
    // empty text, so the no_text reason is `empty` rather than `no_speech`).
    let transcribe = TestTranscribe::returning_empty();
    let inject = TestInject::new();
    let (s, _) = session(transcribe, inject);
    let (outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(
        outcome,
        UtteranceOutcome::NoText { reason: "empty" }
    ));
    let no_text: Vec<_> = parse_events(&bytes)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1);
    assert_eq!(no_text[0]["reason"], "empty");
}

#[test]
fn start_emits_opening_then_recording_in_order() {
    // The supervisor / UI relies on the opening → recording transition
    // shape; the test pins it so a refactor that flips or skips a state
    // is caught here.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    let trace = state_trace(&buf);
    assert_eq!(
        &trace[..2],
        &["opening".to_owned(), "recording".to_owned()],
        "expected opening → recording prefix, got {trace:?}"
    );
}

#[test]
fn stale_cancel_while_idle_is_a_noop() {
    // A cancel that arrives when the session is idle (no recording in
    // flight) must do nothing, emit nothing — Python's
    // `if not self.recording: return`.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    s.cancel(0, &mut buf).expect("cancel-while-idle");
    s.cancel(42, &mut buf)
        .expect("cancel-while-idle (bogus epoch)");
    assert!(buf.is_empty(), "cancel-while-idle must not emit any events");
    assert_eq!(s.state(), SessionState::Idle);
    assert_eq!(s.epoch(), 0);
}

#[test]
fn inject_failure_still_emits_utterance() {
    // Python's `_inject` logs and the utterance event still fires (the
    // user sees the text was decoded, just not pasted). The Rust port
    // matches that and surfaces the inject failure on the utterance
    // event so the supervisor can drive a "couldn't paste" UI without
    // re-parsing logs.
    let transcribe = TestTranscribe::returning_text("hello there");
    let inject = TestInject::failing("clipboard busy");
    let (s, _) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert!(
        s.inject_backend().injected.borrow().is_empty(),
        "the failing inject must NOT record success"
    );
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event must still fire on inject failure");
    assert_eq!(
        utterance["inject_error"],
        "inject backend error: clipboard busy"
    );
}

#[test]
fn epoch_bumps_monotonically_per_start() {
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    let a = s.start(&mut buf).expect("start#1");
    s.stop_and_transcribe(&mut buf).expect("stop#1");
    let b = s.start(&mut buf).expect("start#2");
    s.stop_and_transcribe(&mut buf).expect("stop#2");
    let c = s.start(&mut buf).expect("start#3");
    assert!(
        a < b && b < c,
        "epochs must be monotonically increasing: {a},{b},{c}"
    );
}
