//! Coverage for the session dictionary policy boundary.

use super::tests_support::*;
use super::UtteranceOutcome;

#[test]
fn dictionary_replacements_rewrite_the_transcript_before_injection() {
    // The attached dictionary's deterministic replacement table rewrites the
    // decoded text before it is injected -- Python's `_dictionary_runtime`.
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "hello".to_owned(),
            to: "hi".to_owned(),
        }],
    });
    let (outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    match outcome {
        UtteranceOutcome::Injected { text, .. } => assert_eq!(text, "hi world"),
        other => panic!("expected Injected, got {other:?}"),
    }
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["hi world".to_owned()]
    );
}

#[test]
fn no_dictionary_leaves_the_transcript_unchanged() {
    // Regression guard: a session without a dictionary injects the raw
    // transcript, byte-identical to before the seam existed.
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["hello world".to_owned()]
    );
}

#[test]
fn dictionary_replacement_can_rescue_a_blacklisted_transcript() {
    // Replacements run before classification: STT returns a blacklist phrase ("tak")
    // flagged as a hallucination; a replacement "tak" -> "thanks" is applied
    // BEFORE classification, so the corrected text is re-classified as normal
    // dictation and injected -- Python runs `_dictionary_runtime` before the
    // hallucination check.
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_hallucination("tak");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "tak".to_owned(),
            to: "thanks".to_owned(),
        }],
    });
    let (outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    match outcome {
        UtteranceOutcome::Injected { text, .. } => assert_eq!(text, "thanks"),
        other => panic!("expected Injected (rescued by replacement), got {other:?}"),
    }
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["thanks".to_owned()]
    );
}

#[test]
fn dictionary_replacement_into_a_blacklist_phrase_is_dropped() {
    // The reverse: a replacement rewrites normal dictation INTO a blacklist
    // phrase; re-classification then drops it as no_speech (nothing injected).
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("cheers");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "cheers".to_owned(),
            to: "tak".to_owned(),
        }],
    });
    let (outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(
        matches!(
            outcome,
            UtteranceOutcome::NoText {
                reason: "no_speech"
            }
        ),
        "{outcome:?}"
    );
    assert!(s.inject_backend().injected.borrow().is_empty());
}

#[test]
fn utterance_event_carries_dictionary_replacements() {
    // Every replacement that fires is recorded on the
    // utterance event as `dictionary_replacements` ({from,to,count}), which
    // `ui/log_render.rs` counts and telemetry/history keep.
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "hello".to_owned(),
            to: "hi".to_owned(),
        }],
    });
    let (_outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("an utterance event must be emitted");
    let reps = utterance
        .get("dictionary_replacements")
        .and_then(|v| v.as_array())
        .expect("dictionary_replacements array present when a replacement fired");
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0]["from"], "hello");
    assert_eq!(reps[0]["to"], "hi");
    assert_eq!(reps[0]["count"], 1);
}

#[test]
fn utterance_event_carries_dictionary_terms_from_transcription() {
    let transcribe = TestTranscribe::returning_text("hello world");
    if let super::tests_support::TranscribeOutcome::Ok(result) = &mut *transcribe.next.borrow_mut()
    {
        result.dictionary_terms = Some(vec!["Factus".to_owned(), "Codex".to_owned()].into());
    }
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("an utterance event must be emitted");

    assert_eq!(
        utterance["dictionary_terms"],
        serde_json::json!(["Factus", "Codex"])
    );
    assert!(utterance.get("dictionary_replacements").is_none());
}
