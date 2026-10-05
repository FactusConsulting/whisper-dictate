//! Profile-free sessions must preserve their event contract.

use super::tests_support::*;
use serde_json::Value;

#[test]
fn session_without_matcher_stays_byte_identical() {
    // A session that never opts into the matcher must NOT emit a
    // `state=profile` line and must NOT mutate its SessionConfig. Pins
    // the "opt-in" contract so every pre-profile test in tests_ported
    // tests_transitions keeps its exact event trace.
    let transcribe = TestTranscribe::returning_text("hey");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());

    let events = parse_events(&bytes);
    let has_profile = events
        .iter()
        .any(|e| e.get("state").and_then(Value::as_str) == Some("profile"));
    assert!(
        !has_profile,
        "matcher-less sessions must not emit profile events"
    );
}
