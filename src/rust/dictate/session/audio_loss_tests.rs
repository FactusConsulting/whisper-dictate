use super::RecordingAudioLoss;
use crate::dictate::session::tests_support::{
    one_second_pcm, parse_events, session, TestInject, TestTranscribe, TranscribeOutcome,
};
use crate::dictate::session::{JsonlHistorySink, JsonlMetricsSink, TranscribeResult};

const LOSS: RecordingAudioLoss = RecordingAudioLoss {
    capture_chunks_dropped: 2,
    pipeline_events_dropped: 3,
    pending_frames_dropped: 5,
};

#[test]
fn exact_loss_counts_reach_utterance_history_and_metrics() {
    let (mut session, mut output, _guard) =
        session(TestTranscribe::returning_text("words"), TestInject::new());
    let dir = tempfile::tempdir().unwrap();
    let history = dir.path().join("history.jsonl");
    let metrics = dir.path().join("metrics.jsonl");
    session = session
        .with_history_sink(Box::new(JsonlHistorySink::new(history.clone())))
        .with_metrics_sink(Box::new(JsonlMetricsSink::new(metrics.clone())));
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    assert!(session.begin_transcription());
    let loss = RecordingAudioLoss {
        capture_chunks_dropped: u64::MAX,
        ..LOSS
    };
    session.set_recording_audio_loss(Some(loss));
    session.finish_transcription(&mut output).unwrap();
    let payload = parse_events(&output)
        .into_iter()
        .find(|event| event["event"] == "utterance")
        .unwrap();
    for path in [history, metrics] {
        let row: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(path).unwrap().trim()).unwrap();
        for (key, count) in loss.fields() {
            assert_eq!(payload[key].as_u64(), Some(count));
            assert_eq!(row[key], payload[key]);
        }
    }
    assert!(session.recording_audio_loss.is_none());
}

#[test]
fn capture_metadata_is_absent_for_standalone_sessions() {
    let (mut session, mut output, _guard) =
        session(TestTranscribe::returning_text("words"), TestInject::new());
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    session.stop_and_transcribe(&mut output).unwrap();
    let payload = parse_events(&output)
        .into_iter()
        .find(|event| event["event"] == "utterance")
        .unwrap();
    for (key, _) in LOSS.fields() {
        assert!(payload.get(key).is_none());
    }
}

#[test]
fn injection_failure_keeps_that_utterances_audio_loss() {
    let (mut session, mut output, _guard) = session(
        TestTranscribe::returning_text("words"),
        TestInject::failing("target refused input"),
    );
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    assert!(session.begin_transcription());
    session.set_recording_audio_loss(Some(LOSS));
    session.finish_transcription(&mut output).unwrap();
    let payload = parse_events(&output)
        .into_iter()
        .find(|event| event["event"] == "utterance")
        .unwrap();
    assert!(payload["inject_error"]
        .as_str()
        .unwrap()
        .contains("target refused input"));
    for (key, count) in LOSS.fields() {
        assert_eq!(payload[key].as_u64(), Some(count));
    }
    assert!(session.recording_audio_loss.is_none());
}

#[test]
fn rejected_recordings_clear_loss_before_a_later_healthy_utterance() {
    for (backend, frames) in [
        (TestTranscribe::returning_empty(), one_second_pcm()),
        (
            TestTranscribe::returning_error("STT failed"),
            one_second_pcm(),
        ),
        (TestTranscribe::returning_text("unused"), Vec::new()),
        (TestTranscribe::returning_text("unused"), vec![0.0; 1]),
    ] {
        let (mut session, mut output, _guard) = session(backend, TestInject::new());
        session.start(&mut output).unwrap();
        session.push_frame(&frames);
        assert!(session.begin_transcription());
        session.set_recording_audio_loss(Some(LOSS));
        session.finish_transcription(&mut output).unwrap();
        assert!(parse_events(&output)
            .iter()
            .all(|event| event["event"] != "utterance"));
        assert!(session.recording_audio_loss.is_none());

        *session.transcribe_backend().next.borrow_mut() = TranscribeOutcome::Ok(TranscribeResult {
            text: "healthy".to_owned(),
            ..Default::default()
        });
        output.clear();
        session.start(&mut output).unwrap();
        session.push_frame(&one_second_pcm());
        assert!(session.begin_transcription());
        session.set_recording_audio_loss(Some(RecordingAudioLoss::default()));
        session.finish_transcription(&mut output).unwrap();
        let payload = parse_events(&output)
            .into_iter()
            .find(|event| event["event"] == "utterance")
            .unwrap();
        for (key, _) in LOSS.fields() {
            assert_eq!(payload[key], 0);
        }
    }
}

#[test]
fn failed_event_output_still_clears_recording_loss() {
    struct BrokenWriter;
    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed output"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (mut session, mut output, _guard) =
        session(TestTranscribe::returning_text("words"), TestInject::new());
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    assert!(session.begin_transcription());
    session.set_recording_audio_loss(Some(LOSS));
    assert!(session.finish_transcription(&mut BrokenWriter).is_err());
    assert!(session.recording_audio_loss.is_none());
}
