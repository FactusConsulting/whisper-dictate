use super::tests_support::{session, RecordingCueSink, TestInject, TestTranscribe};
use super::SessionState;
use crate::dictate::feedback::CueKind;

#[test]
fn transcription_claim_only_changes_state_and_defers_external_work() {
    let (mut session, mut output, _guard) =
        session(TestTranscribe::returning_text("hello"), TestInject::new());
    let (cue, played) = RecordingCueSink::new();
    session = session.with_cue_sink(Box::new(cue));
    assert!(!session.begin_transcription());
    assert_eq!(session.state(), SessionState::Idle);
    session.start(&mut output).unwrap();
    session.push_frame(&super::tests_support::one_second_pcm());
    output.clear();
    assert!(session.begin_transcription());
    assert_eq!(session.state(), SessionState::Transcribing { id: 1 });
    assert!(output.is_empty());
    assert_eq!(*played.lock().unwrap(), [CueKind::Start]);
    assert!(session
        .transcribe_backend()
        .seen_pcm_len
        .borrow()
        .is_empty());
    assert!(
        !session.begin_transcription(),
        "an utterance is accepted only once"
    );
    session.finish_transcription(&mut output).unwrap();
    assert_eq!(session.state(), SessionState::Idle);
    assert_eq!(*played.lock().unwrap(), [CueKind::Start, CueKind::Stop]);
    assert_eq!(*session.transcribe_backend().seen_pcm_len.borrow(), [16000]);
}
