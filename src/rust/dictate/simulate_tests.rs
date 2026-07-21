//! Tests for [`super`] — the offline session-drive helper. Hermetic: a stub
//! transcribe backend stands in for the cloud call, so no network or model
//! is touched. The CLI handler's cloud wiring is covered live by
//! `scripts/integration/groq-cli-smoke.sh`.

use super::*;
use crate::dictate::{TranscribeError, TranscribeResult};
use crate::test_env_lock::ENV_LOCK;

/// Canned transcribe backend returning a fixed transcript.
struct StubTranscribe {
    text: &'static str,
}

impl TranscribeBackend for StubTranscribe {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        Ok(TranscribeResult {
            text: self.text.to_owned(),
            duration_s: pcm.len() as f64 / f64::from(sample_rate.max(1)),
            ..Default::default()
        })
    }
}

fn session_with(text: &'static str) -> DictateSession<StubTranscribe, CaptureInject> {
    DictateSession::new(
        StubTranscribe { text },
        CaptureInject::default(),
        SessionConfig::default(),
    )
}

#[test]
fn drive_injects_transcribed_text_and_streams_events_when_gated() {
    // Worker events are gated behind VOICEPI_WORKER_EVENTS (so an ungated CLI
    // drive doesn't leak lines); serialise env access via ENV_LOCK.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");
    let mut session = session_with("hello world");
    let mut events = Vec::new();
    // 1.0 s of audio at 16 kHz — above the default 0.5 s min-record floor.
    let pcm = vec![0.1_f32; 16_000];
    let outcome = drive_session_over_pcm(&mut session, &pcm, &mut events).expect("drive ok");
    std::env::remove_var("VOICEPI_WORKER_EVENTS");

    match outcome {
        UtteranceOutcome::Injected { text, .. } => assert_eq!(text, "hello world"),
        other => panic!("expected Injected, got {other:?}"),
    }
    // With the gate on, the session streamed its worker events onto the
    // writer, including the final utterance carrying the transcript.
    let stream = String::from_utf8_lossy(&events);
    assert!(
        stream.contains("\"event\":\"utterance\"") && stream.contains("hello world"),
        "expected an utterance event with the transcript, got: {stream}"
    );
}

#[test]
fn drive_does_not_leak_events_without_the_gate() {
    // Regression guard: without VOICEPI_WORKER_EVENTS the drive must not
    // write any worker-event lines to the writer (still injects, though).
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("VOICEPI_WORKER_EVENTS");
    let mut session = session_with("hello world");
    let mut events = Vec::new();
    let outcome = drive_session_over_pcm(&mut session, &vec![0.1_f32; 16_000], &mut events)
        .expect("drive ok");
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert!(events.is_empty(), "no events must leak without the gate");
}

#[test]
fn drive_empty_pcm_resolves_to_no_audio() {
    let mut session = session_with("unused");
    let mut events = Vec::new();
    let outcome = drive_session_over_pcm(&mut session, &[], &mut events).expect("drive ok");
    assert!(
        matches!(outcome, UtteranceOutcome::NoAudio),
        "empty pcm must resolve to NoAudio, got {outcome:?}"
    );
}

#[test]
fn capture_inject_records_texts_in_order() {
    let capture = CaptureInject::default();
    capture.inject("first").expect("inject");
    capture.inject("second").expect("inject");
    assert_eq!(
        capture.injected(),
        vec!["first".to_owned(), "second".to_owned()]
    );
}

#[test]
fn config_reads_format_commands_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var(FORMAT_COMMANDS_ENV, "en");
    assert_eq!(
        simulate_session_config().format_command_set.as_deref(),
        Some("en")
    );
    std::env::set_var(FORMAT_COMMANDS_ENV, "   ");
    assert_eq!(
        simulate_session_config().format_command_set,
        None,
        "blank normalises to None"
    );
    std::env::remove_var(FORMAT_COMMANDS_ENV);
    assert_eq!(simulate_session_config().format_command_set, None);
}
