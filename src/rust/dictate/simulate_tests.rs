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
fn drive_session_cycles_reuses_one_session_across_presses() {
    // Regression guard (harness side) for the "PTT only worked the first
    // time, then got stuck" bug: driving N cycles over the SAME session must
    // inject on EVERY cycle, not just the first. A session that failed to
    // reset would make the 2nd `start()` return `AlreadyActive`, surfacing
    // as an Err from `drive_session_cycles` — so reaching three Injected
    // outcomes proves the session re-arms press after press.
    let mut session = session_with("hello world");
    let mut sink = std::io::sink();
    let pcm = vec![0.1_f32; 16_000];
    let outcomes = drive_session_cycles(&mut session, &pcm, &mut sink, 3).expect("three cycles ok");
    assert_eq!(outcomes.len(), 3, "one outcome per cycle");
    for (i, outcome) in outcomes.iter().enumerate() {
        assert!(
            matches!(outcome, UtteranceOutcome::Injected { text, .. } if text == "hello world"),
            "cycle {} must inject the transcript, got {outcome:?}",
            i + 1
        );
    }
    // The capture backend accumulated one injection per cycle, in order.
    assert_eq!(
        session.inject_backend().injected(),
        vec![
            "hello world".to_owned(),
            "hello world".to_owned(),
            "hello world".to_owned()
        ]
    );
}

#[test]
fn drive_session_cycles_treats_zero_repeat_as_one() {
    let mut session = session_with("once");
    let mut sink = std::io::sink();
    let outcomes = drive_session_cycles(&mut session, &vec![0.1_f32; 16_000], &mut sink, 0)
        .expect("zero-repeat drives one cycle");
    assert_eq!(outcomes.len(), 1, "repeat=0 collapses to a single cycle");
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
fn to_clean_jsonl_strips_prefix_and_yields_valid_json() {
    // Real wire-format lines (prefixed) + a blank line, as the session emits.
    let raw = "[worker-event] {\"event\":\"status\",\"state\":\"opening\"}\n\
               \n\
               [worker-event] {\"event\":\"utterance\",\"text\":\"hi\"}\n";
    let out = to_clean_jsonl(raw);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "blank line dropped; got {out:?}");
    for line in &lines {
        assert!(
            !line.starts_with("[worker-event]"),
            "prefix must be stripped: {line}"
        );
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("line is not valid JSON ({e}): {line}"));
    }
}

#[test]
fn to_clean_jsonl_passes_through_unprefixed_and_empty() {
    assert_eq!(to_clean_jsonl("{\"a\":1}"), "{\"a\":1}");
    assert_eq!(to_clean_jsonl(""), "");
    assert_eq!(to_clean_jsonl("   \n\n"), "");
}

fn cloud_cfg(base_url: &str) -> CloudTranscribeConfig {
    CloudTranscribeConfig {
        base_url: base_url.to_owned(),
        api_key: "test-key".to_owned(),
        model: "whisper-large-v3-turbo".to_owned(),
        timeout_ms: 100,
        language: None,
        prompt: None,
    }
}

#[test]
fn resolve_cloud_transcribe_errors_on_empty_config() {
    let empty = CloudTranscribeConfig {
        api_key: String::new(),
        model: String::new(),
        ..cloud_cfg("https://api.groq.com/openai/v1")
    };
    match resolve_cloud_transcribe(empty, false) {
        Ok(_) => panic!("empty config must error"),
        Err(e) => assert!(e.to_string().contains("cloud STT"), "{e}"),
    }
}

#[test]
fn resolve_cloud_transcribe_blocks_remote_under_local_only() {
    match resolve_cloud_transcribe(cloud_cfg("https://api.groq.com/openai/v1"), true) {
        Ok(_) => panic!("remote endpoint under local-only must error"),
        Err(e) => assert!(e.to_string().contains("LOCAL_ONLY"), "{e}"),
    }
}

#[test]
fn resolve_cloud_transcribe_ok_remote_when_not_local_only() {
    let backend = resolve_cloud_transcribe(cloud_cfg("https://api.groq.com/openai/v1"), false)
        .expect("remote allowed when local-only off");
    assert_eq!(backend.config().base_url, "https://api.groq.com/openai/v1");
}

#[test]
fn resolve_cloud_transcribe_allows_loopback_under_local_only() {
    let backend = resolve_cloud_transcribe(cloud_cfg("http://127.0.0.1:1234/v1"), true)
        .expect("loopback allowed under local-only");
    assert_eq!(backend.config().base_url, "http://127.0.0.1:1234/v1");
}

#[test]
fn handle_simulate_session_errors_without_cloud_config() {
    // Cover the CLI handler entry + error propagation deterministically: with
    // no cloud STT env configured, it must fail fast (before any decode /
    // network) with the actionable message.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for key in [
        "VOICEPI_STT_MODEL",
        "VOICEPI_STT_API_KEY",
        "GROQ_API_KEY",
        "OPENAI_API_KEY",
        "VOICEPI_STT_BASE_URL",
    ] {
        std::env::remove_var(key);
    }
    match handle_simulate_session("does-not-matter.wav", false, 1) {
        Ok(()) => panic!("must error without a configured cloud backend"),
        Err(e) => assert!(e.to_string().contains("cloud STT"), "{e}"),
    }
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
