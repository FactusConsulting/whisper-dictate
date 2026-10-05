//! Coverage for the session recording boundary.

use super::tests_support::*;
use super::{SessionConfig, SessionError, SessionState, UtteranceOutcome};

#[test]
fn push_frame_while_idle_is_dropped() {
    // Frames pushed outside Recording are discarded — the
    // `if self.recording` ingestion gate.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.push_frame(&one_second_pcm());
    // Now actually run a recording with no real frames pushed — the
    // session must see an empty buffer (NoAudio outcome).
    s.start(&mut buf).expect("start");
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert_eq!(outcome, UtteranceOutcome::NoAudio);
}

#[test]
fn direct_session_audio_is_truncated_at_the_live_max_record_cap() {
    let transcribe = TestTranscribe::returning_text("bounded");
    let inject = TestInject::new();
    let (mut session, mut output, _guard) = session(transcribe, inject);
    let _snapshot = EnvVarSnapshot::new(&["VOICEPI_MAX_RECORD_S"]);
    std::env::set_var("VOICEPI_MAX_RECORD_S", "0.6");

    session.start(&mut output).expect("start");
    session.push_frame(&vec![0.1; 7_000]);
    session.push_frame(&vec![0.2; 7_000]);
    session
        .stop_and_transcribe(&mut output)
        .expect("bounded transcription");

    assert_eq!(
        *session.transcribe_backend().seen_pcm_len.borrow(),
        vec![9_600],
        "the native pump must keep audio through the cap and discard everything beyond it"
    );
}

#[test]
fn session_owned_recording_cap_does_not_require_process_env() {
    let transcribe = TestTranscribe::returning_text("bounded");
    let inject = TestInject::new();
    let config = SessionConfig {
        max_record_seconds: Some(0.6),
        ..SessionConfig::default()
    };
    let (mut session, mut output, _guard) = session_with_config(transcribe, inject, config);

    session.start(&mut output).unwrap();
    session.push_frame(&vec![0.1; 14_000]);
    session.stop_and_transcribe(&mut output).unwrap();

    assert_eq!(
        *session.transcribe_backend().seen_pcm_len.borrow(),
        vec![9_600]
    );
}

#[test]
fn explicit_zero_recording_cap_disables_the_ambient_safety_ceiling() {
    let transcribe = TestTranscribe::returning_text("uncapped");
    let inject = TestInject::new();
    let config = SessionConfig {
        max_record_seconds: Some(0.0),
        ..SessionConfig::default()
    };
    let (mut session, mut output, _guard) = session_with_config(transcribe, inject, config);

    session.start(&mut output).unwrap();
    session.push_frame(&vec![0.1; 2_000_000]);
    session.stop_and_transcribe(&mut output).unwrap();

    assert_eq!(
        *session.transcribe_backend().seen_pcm_len.borrow(),
        vec![2_000_000]
    );
}

#[test]
fn start_while_active_is_an_error() {
    // An early-return on start; the Rust port returns
    // `AlreadyActive` so a buggy caller can't accidentally skip a
    // recording. The state must NOT change and the epoch must NOT bump.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
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
    // `_stop_and_transcribe` early-returns on `not self.recording`;
    // the Rust port surfaces that as `NotRecording` with no
    // events emitted.
    let transcribe = TestTranscribe::returning_text("never");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert_eq!(outcome, UtteranceOutcome::NotRecording);
    assert!(buf.is_empty(), "stop-when-idle must not emit any events");
}

#[test]
fn start_emits_opening_then_recording_in_order() {
    // The supervisor / UI relies on the opening → recording transition
    // shape; the test pins it so a refactor that flips or skips a state
    // is caught here.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    let trace = state_trace(&buf);
    assert_eq!(
        &trace[..2],
        &["opening".to_owned(), "recording".to_owned()],
        "expected opening → recording prefix, got {trace:?}"
    );
}
