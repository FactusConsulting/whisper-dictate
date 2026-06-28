//! Characterisation tests ported from
//! `src/python/tests/test_dictate_loop.py`. One Rust counterpart per
//! load-bearing Python test:
//!
//! | Python                                                  | Rust                                            |
//! | ------------------------------------------------------- | ----------------------------------------------- |
//! | `test_full_utterance_is_transcribed_and_injected`       | [`full_utterance_is_transcribed_and_injected`]  |
//! | `test_too_short_capture_is_skipped`                     | [`too_short_capture_is_skipped`]                |
//! | `test_hallucination_is_filtered_and_not_injected`       | [`hallucination_is_filtered_and_not_injected`]  |
//! | `test_no_frames_emits_no_text_no_audio`                 | [`no_frames_emits_no_text_no_audio`]            |
//! | `test_cancel_matching_epoch_discards`                   | [`cancel_matching_epoch_discards`]              |
//! | `test_stale_cancel_for_old_epoch_noops`                 | [`stale_cancel_for_old_epoch_does_not_discard`] |

use super::tests_support::*;
use super::{SessionState, UtteranceOutcome};

#[test]
fn full_utterance_is_transcribed_and_injected() {
    // Python: `test_full_utterance_is_transcribed_and_injected`.
    let transcribe = TestTranscribe::returning_text("hej verden");
    let inject = TestInject::new();
    let (s, _) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());

    match &outcome {
        UtteranceOutcome::Injected { text, result } => {
            assert_eq!(text, "hej verden");
            assert_eq!(result.latency_ms, 42);
        }
        other => panic!("expected Injected, got {other:?}"),
    }
    assert_eq!(
        *s.inject_backend().injected.borrow(),
        vec!["hej verden".to_owned()]
    );
    assert_eq!(s.state(), SessionState::Idle);

    let events = parse_events(&bytes);
    let utterance = events
        .iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("an utterance event must be emitted");
    assert_eq!(utterance["text"], "hej verden");
}

#[test]
fn too_short_capture_is_skipped() {
    // Python: `test_too_short_capture_is_skipped`. 1000 samples is
    // well below the 0.3 s floor — Python drops it as `too_short`.
    let transcribe = TestTranscribe::returning_text("ignored");
    let inject = TestInject::new();
    let (s, _) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &vec![0.0_f32; 1000]);

    assert!(
        matches!(
            outcome,
            UtteranceOutcome::Skipped {
                reason: "too_short"
            }
        ),
        "expected too_short Skipped, got {outcome:?}"
    );
    assert!(s.inject_backend().injected.borrow().is_empty());

    let no_text: Vec<_> = parse_events(&bytes)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1);
    assert_eq!(no_text[0]["reason"], "too_short");
    // Mirror the Python test: recording_s must be reported for too_short
    // (so the user sees how long they held).
    assert!(no_text[0].get("recording_s").is_some());
}

#[test]
fn hallucination_is_filtered_and_not_injected() {
    // Python: `test_hallucination_is_filtered_and_not_injected`.
    let transcribe = TestTranscribe::returning_hallucination("thank you");
    let inject = TestInject::new();
    let (s, _) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert!(
        matches!(
            outcome,
            UtteranceOutcome::NoText {
                reason: "no_speech"
            }
        ),
        "expected no_speech NoText for hallucination, got {outcome:?}"
    );
    assert!(s.inject_backend().injected.borrow().is_empty());

    let no_text: Vec<_> = parse_events(&bytes)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1);
    assert_eq!(no_text[0]["reason"], "no_speech");
}

#[test]
fn no_frames_emits_no_text_no_audio() {
    // Python: `test_no_frames_emits_no_text_no_audio`. The session
    // starts, no frames are pushed, then stop_and_transcribe runs.
    let transcribe = TestTranscribe::returning_text("should not run");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");

    assert_eq!(outcome, UtteranceOutcome::NoAudio);
    assert!(s.inject_backend().injected.borrow().is_empty());

    let no_text: Vec<_> = parse_events(&buf)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1, "expected one no_text event");
    assert_eq!(no_text[0]["reason"], "no_audio");
    // The backend must NOT be consulted on the no-audio path.
    assert!(s.transcribe_backend().seen_pcm_len.borrow().is_empty());
}

#[test]
fn cancel_matching_epoch_discards() {
    // Python: `test_cancel_matching_epoch_discards`. The session is
    // recording; a cancel arrives stamped with the CURRENT epoch and
    // discards the in-flight clip (no transcribe, no inject).
    let transcribe = TestTranscribe::returning_text("should never inject");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    let epoch = s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());

    s.cancel(epoch, &mut buf).expect("cancel");

    assert_eq!(s.state(), SessionState::Idle);
    assert!(s.inject_backend().injected.borrow().is_empty());
    assert!(
        s.transcribe_backend().seen_pcm_len.borrow().is_empty(),
        "transcribe must not run on a matching-epoch cancel"
    );
    let trace = state_trace(&buf);
    assert!(
        trace.iter().any(|s| s == "cancelled"),
        "trace was {trace:?}; expected a cancelled state"
    );
}

#[test]
fn stale_cancel_for_old_epoch_does_not_discard() {
    // Python: `test_stale_cancel_for_old_epoch_noops`. The exact
    // chord-cancel race: epoch N's cancel is delayed past release +
    // re-press, so when it fires the active epoch is N+1. The session
    // MUST NOT discard the new recording.
    //
    // Drive the race explicitly: start, push, stop (epoch 1 done),
    // start again (epoch 2 active), then fire the STALE cancel(1).
    let transcribe = TestTranscribe::returning_text("epoch one text");
    let inject = TestInject::new();
    let (mut s, mut buf) = session(transcribe, inject);
    let first = s.start(&mut buf).expect("start#1");
    s.push_frame(&one_second_pcm());
    s.stop_and_transcribe(&mut buf).expect("stop#1");
    assert_eq!(
        s.inject_backend().injected.borrow().len(),
        1,
        "epoch 1 must succeed"
    );

    // The chord-cancel daemon thread was holding the stale `first`
    // epoch this whole time; meanwhile a new press has opened epoch 2.
    let second = s.start(&mut buf).expect("start#2");
    assert_ne!(first, second, "epoch must bump on every start()");
    s.push_frame(&one_second_pcm());

    // STALE cancel: targets `first` while `second` is the active epoch.
    s.cancel(first, &mut buf).expect("stale cancel");

    assert_eq!(
        s.state(),
        SessionState::Recording { id: second },
        "stale cancel must not tear down the new recording"
    );
    // And finishing the new utterance MUST inject normally — the buffer
    // was untouched by the stale cancel.
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop#2");
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        s.inject_backend().injected.borrow().len(),
        2,
        "epoch 2 must also inject"
    );
}
