//! Coverage for the session transcription boundary.

use super::tests_support::*;
use super::{SessionConfig, UtteranceOutcome};
use crate::dictate::feedback::CueKind;

#[test]
fn failed_transcription_still_emits_processing_done_cue() {
    let (mut session, mut output, _guard) = session(
        TestTranscribe::returning_error("model unavailable"),
        TestInject::new(),
    );
    let (sink, played) = RecordingCueSink::new();
    session = session.with_cue_sink(Box::new(sink));
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    let outcome = session.stop_and_transcribe(&mut output).unwrap();
    assert!(matches!(outcome, UtteranceOutcome::NoText { .. }));
    assert_eq!(
        *played.lock().unwrap(),
        [CueKind::Start, CueKind::Stop, CueKind::Done]
    );
}

#[test]
fn transcribe_error_emits_no_text_no_speech() {
    // Any model error surfaces
    // as `reason="no_speech"` on the no-text event.
    let transcribe = TestTranscribe::returning_error("model panicked");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
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
    // An empty `result.text` maps to
    // `reason="empty"` (the `is_hallucination` flag is irrelevant on an
    // empty text, so the no_text reason is `empty` rather than `no_speech`).
    let transcribe = TestTranscribe::returning_empty();
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
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
fn inject_failure_still_emits_utterance() {
    // The inject failure is logged and the utterance event still fires
    // (the user sees the text was decoded, just not pasted). The failure
    // surfaces on the utterance
    // event so the supervisor can drive a "couldn't paste" UI without
    // re-parsing logs.
    let transcribe = TestTranscribe::returning_text("hello there");
    let inject = TestInject::failing("clipboard busy");
    let (s, _, _guard) = session(transcribe, inject);
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
fn injecting_status_is_emitted_before_the_final_utterance() {
    let transcribe = TestTranscribe::returning_text("ready to inject");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());

    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    let trace = state_trace(&bytes);
    assert!(trace.iter().any(|state| state == "injecting"));
    let events = parse_events(&bytes);
    let injecting = events
        .iter()
        .position(|event| event.get("state").and_then(|v| v.as_str()) == Some("injecting"))
        .expect("injecting status");
    let utterance = events
        .iter()
        .position(|event| event.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert!(injecting < utterance, "injecting must precede utterance");
}

#[test]
fn post_process_runs_before_format_commands() {
    // Order fidelity: `postprocess -> format -> inject`. The
    // post-processor emits a transcript CONTAINING a spoken format
    // command; with format_command_set = `en` that command must then be
    // applied to the post-processor's OUTPUT (proving postprocess ran
    // first), yielding the final injected text.
    let transcribe = TestTranscribe::returning_text("noisy input");
    let inject = TestInject::new();
    let config = SessionConfig {
        format_command_set: Some("en".to_owned()),
        ..SessionConfig::default()
    };
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let s = s.with_post_process(Box::new(TestPostProcess::returning("done new line here")));
    let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["done\nhere".to_owned()],
        "format commands must apply to the post-processor's output \
         (proves postprocess ran before format)",
    );
}

#[test]
fn transcribing_state_fires_even_on_no_audio() {
    // `state="transcribing"` BEFORE the empty-frames guard so the UI
    // sequence is `recording -> transcribing -> no_text -> ready` even
    // on a no-audio recording. Without this the Rust path would jump
    // straight from `recording` to `no_text`.
    let transcribe = TestTranscribe::returning_text("never runs");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    s.stop_and_transcribe(&mut buf).expect("stop");

    let trace = state_trace(&buf);
    let transcribing_idx = trace
        .iter()
        .position(|s| s == "transcribing")
        .expect("transcribing state must fire even on no-audio");
    let no_text_idx = trace.iter().position(|s| s == "no_text").expect("no_text");
    assert!(
        transcribing_idx < no_text_idx,
        "transcribing must precede no_text: trace was {trace:?}",
    );
}
