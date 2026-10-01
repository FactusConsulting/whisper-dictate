//! Coverage for the session builders boundary.

use super::tests_support::*;
use super::UtteranceOutcome;

#[test]
fn with_optional_dictionary_preserves_replacements_and_empty_dictionaries() {
    use crate::dictionary::{Dictionary, Replacement, SessionDictionary};

    {
        let with = SessionDictionary {
            dictionary: Dictionary {
                terms: Vec::new(),
                replacements: vec![Replacement {
                    from: "hello".to_owned(),
                    to: "hi".to_owned(),
                }],
            },
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        let (s, _, _guard) = session(
            TestTranscribe::returning_text("hello world"),
            TestInject::new(),
        );
        let s = s.with_optional_dictionary(with);
        let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
        assert_eq!(
            s.inject_backend().injected.borrow().as_slice(),
            ["hi world".to_owned()]
        );
    }

    {
        let without = SessionDictionary {
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            enabled: false,
        };
        let (s2, _, _guard2) = session(
            TestTranscribe::returning_text("hello world"),
            TestInject::new(),
        );
        let s2 = s2.with_optional_dictionary(without);
        let (_outcome2, _bytes2, s2) = run_one_utterance(s2, &one_second_pcm());
        assert_eq!(
            s2.inject_backend().injected.borrow().as_slice(),
            ["hello world".to_owned()]
        );
    }
}

#[test]
fn default_session_uses_the_silent_cue_sink() {
    // Regression guard: without `.with_cue_sink(...)` the session must
    // NOT touch the audio subsystem, so the huge existing test surface
    // stays sound-free and the type stays byte-for-byte compatible
    // with pre-cue callers. Strongest signal available without
    // introspection into the default sink: a full utterance completes
    // without panicking (the default NoOpCueSink drop-in never
    // touches cpal / kernel32 / paplay).
    let transcribe = TestTranscribe::returning_text("hi");
    let inject = TestInject::new();
    let (s, _buf, _guard) = session(transcribe, inject);
    let (outcome, _bytes, _s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
}

#[test]
fn default_session_uses_the_no_op_ducker() {
    // Regression guard: without `.with_ducker(...)` the session must
    // NOT read env vars or touch WASAPI -- the default `NoOpAudioDucker`
    // has to be silent so the huge existing test surface stays
    // dependency-free. The strongest signal available without
    // introspection into the default ducker is that a full utterance
    // completes without panicking.
    let transcribe = TestTranscribe::returning_text("hi");
    let inject = TestInject::new();
    let (s, _buf, _guard) = session(transcribe, inject);
    let (outcome, _bytes, _s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
}
