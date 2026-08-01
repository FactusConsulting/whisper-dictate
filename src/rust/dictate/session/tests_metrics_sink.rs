//! Integration tests for the `MetricsSink` seam on [`super::DictateSession`].
//!
//! Unit tests for the sink implementation itself (unfiltered write, IO
//! error handling, path expansion) live in
//! [`super::metrics_sink::tests`]. These tests pin the WIRING: an attached
//! sink receives the same payload the worker-event emitter just wrote,
//! non-attached sessions are a no-op, and a broken sink cannot abort a
//! dictation. Together they establish parity with Python's
//! `vp_dictate._record_utterance_event`, which calls `_emit_worker_event`
//! AND `append_record_sinks(metrics_jsonl=..., json_output=...)` from the
//! same event dict.
//!
//! The shape closely tracks [`super::tests_history_sink`] because the two
//! sinks share a wire-up seam (`record_sinks`); the differences that
//! justify a separate file:
//!  * the metrics sink writes the FULL event (no allow-list filter), so
//!    the on-disk assertions include fields history would drop
//!    (`compute_ms`, arbitrary future fields),
//!  * metrics has no reader helper like `crate::history::read_rows`, so
//!    the round-trip test parses the JSONL by hand.

use std::cell::RefCell;
use std::fs;
use std::sync::Mutex;

use serde_json::Value;

use super::tests_support::*;
use super::{JsonlMetricsSink, MetricsSink, NoopMetricsSink, UtteranceOutcome};

// A capturing sink that records every payload it saw so tests can assert
// EXACTLY what the session handed to `append`. `Mutex<RefCell<...>>`
// because the trait's boxed variant requires `Send` -- the real production
// sink is stateless so the box IS Send, but a plain `RefCell` isn't.
struct CapturingSink {
    seen: Mutex<RefCell<Vec<Value>>>,
}

impl CapturingSink {
    fn new() -> Self {
        Self {
            seen: Mutex::new(RefCell::new(Vec::new())),
        }
    }

    fn snapshot(&self) -> Vec<Value> {
        self.seen.lock().unwrap().borrow().clone()
    }
}

impl MetricsSink for CapturingSink {
    fn append(&self, event: &Value) {
        self.seen.lock().unwrap().borrow_mut().push(event.clone());
    }
}

/// Shared-Arc wrapper so the test can retain a handle for `snapshot()`
/// while the session takes ownership of the boxed sink.
struct SharedSink(std::sync::Arc<CapturingSink>);
impl MetricsSink for SharedSink {
    fn append(&self, event: &Value) {
        self.0.append(event);
    }
}

fn shared_sink() -> (std::sync::Arc<CapturingSink>, Box<dyn MetricsSink + Send>) {
    let inner = std::sync::Arc::new(CapturingSink::new());
    let boxed: Box<dyn MetricsSink + Send> = Box::new(SharedSink(std::sync::Arc::clone(&inner)));
    (inner, boxed)
}

/// Handing the same-shaped payload as the worker-event emitter -- with
/// `ts` and `text` fields -- verifies that the session hooks
/// `record_sinks` into the successful-utterance branch for the metrics
/// fan-out too.
#[test]
fn successful_utterance_calls_metrics_sink() {
    let transcribe = TestTranscribe::returning_text("hej verden");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);

    let (captured, sink) = shared_sink();
    s = s.with_metrics_sink(sink);

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");

    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "successful utterance expected: {outcome:?}"
    );
    let seen = captured.snapshot();
    assert_eq!(
        seen.len(),
        1,
        "metrics sink must see exactly one payload per successful utterance"
    );
    let row = &seen[0];
    assert_eq!(row["event"], "utterance");
    assert_eq!(row["text"], "hej verden");
    // The metrics sink is UNFILTERED — timing/quality fields the history
    // sink also carries must round-trip verbatim here.
    assert!(
        row["ts"].is_number(),
        "ts must be present (Python `_base_event` parity)"
    );
    assert!(row["compute_ms"].is_number(), "compute_ms must be present");
    assert!(
        row["audio_duration_s"].is_number(),
        "audio_duration_s must be present"
    );
}

/// Inject-failure branch: Python's `_record_utterance_event` still fires
/// the event dict through `append_record_sinks` (BOTH sinks together), so
/// the Rust metrics sink must also see the payload — with the
/// `inject_error` field carrying the failure reason. This pins the second
/// wire-up call site (the inject-error branch of `run_transcription`).
#[test]
fn inject_failure_still_calls_metrics_sink() {
    let transcribe = TestTranscribe::returning_text("hej verden");
    let inject = TestInject::failing("no display");
    let (mut s, mut buf, _guard) = session(transcribe, inject);

    let (captured, sink) = shared_sink();
    s = s.with_metrics_sink(sink);

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let _ = s.stop_and_transcribe(&mut buf).expect("stop");

    let seen = captured.snapshot();
    assert_eq!(
        seen.len(),
        1,
        "sink must see the utterance on inject failure"
    );
    assert_eq!(seen[0]["text"], "hej verden");
    assert_eq!(seen[0]["inject_error"], "inject backend error: no display");
}

/// A session without a metrics sink attached MUST NOT touch the sink —
/// verified indirectly by the fact that no sink method could have run.
/// Attaching the noop sink and running an utterance also stays crash-free
/// (parity spec: the noop is truly noop).
#[test]
fn session_without_metrics_sink_is_noop() {
    let transcribe = TestTranscribe::returning_text("no sink");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    // No `.with_metrics_sink(...)` call.
    let (outcome, _bytes, _s) = run_one_utterance(s, &one_second_pcm());
    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "session without sink must still complete normally"
    );
}

#[test]
fn noop_metrics_sink_is_still_wired_through_without_panic() {
    let transcribe = TestTranscribe::returning_text("noop attached");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_metrics_sink(Box::new(NoopMetricsSink));
    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
}

/// End-to-end round trip: session -> production JSONL sink -> disk ->
/// re-parse. This is the acceptance test for the parity blocker this PR
/// fixes: if a JSONL-parsing consumer can pull the row the session wrote,
/// the user's external metrics tooling keeps working when the default
/// engine flips to Rust.
#[test]
fn round_trip_session_to_disk_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.jsonl");

    let transcribe = TestTranscribe::returning_text("hello from rust engine");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_metrics_sink(Box::new(JsonlMetricsSink::new(path.clone())));

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let _ = s.stop_and_transcribe(&mut buf).expect("stop");

    let raw = fs::read_to_string(&path).unwrap();
    // Valid JSONL: compact, newline-terminated, exactly one row.
    assert!(raw.ends_with('\n'));
    assert_eq!(raw.lines().count(), 1, "one utterance -> one JSONL row");
    let row: Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(row["text"], "hello from rust engine");
    assert_eq!(row["event"], "utterance");
    assert!(
        row["ts"].is_number(),
        "ts must be present so downstream tooling can order rows"
    );
    // The metrics sink is UNFILTERED — assert one of the fields the
    // history sink WOULD have dropped (via the `history_event`
    // allow-list). `compute_ms` is emitted by `wire::emit_utterance` but
    // is NOT on the `HISTORY_KEYS` allow-list, so its presence here
    // proves the two sinks diverge as designed.
    assert!(
        row["compute_ms"].is_number(),
        "metrics sink must preserve non-history fields like compute_ms"
    );
}

/// Both sinks attached together — the Python fan-out shape. The metrics
/// row must carry `compute_ms` (unfiltered), the history row must NOT
/// (filtered). This pins the two-sink parity contract in one test: the
/// same payload lands in BOTH sinks, but each applies its own filter.
#[test]
fn metrics_and_history_sinks_both_receive_the_same_payload() {
    let dir = tempfile::tempdir().unwrap();
    let metrics_path = dir.path().join("metrics.jsonl");
    let history_path = dir.path().join("history.jsonl");

    let transcribe = TestTranscribe::returning_text("dual sinks");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s
        .with_metrics_sink(Box::new(JsonlMetricsSink::new(metrics_path.clone())))
        .with_history_sink(Box::new(super::JsonlHistorySink::new(history_path.clone())));

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let _ = s.stop_and_transcribe(&mut buf).expect("stop");

    let metrics_row: Value =
        serde_json::from_str(fs::read_to_string(&metrics_path).unwrap().trim()).unwrap();
    let history_row: Value =
        serde_json::from_str(fs::read_to_string(&history_path).unwrap().trim()).unwrap();

    // Both sinks got the same text and event tag.
    assert_eq!(metrics_row["text"], "dual sinks");
    assert_eq!(history_row["text"], "dual sinks");
    assert_eq!(metrics_row["event"], "utterance");
    assert_eq!(history_row["event"], "utterance");

    // Divergence: `compute_ms` is on the metrics row (unfiltered) but
    // NOT on the history row (history's allow-list only includes
    // `compute_s`, not `compute_ms`).
    assert!(
        metrics_row["compute_ms"].is_number(),
        "metrics must preserve compute_ms"
    );
    assert!(
        history_row.get("compute_ms").is_none(),
        "history's allow-list must have dropped compute_ms"
    );
}

/// #606  4: the metrics utterance row must carry the
/// FULL schema Python's `_utterance_event` emits, not the trimmed
/// subset the first cut wrote. This test pins the presence of every
/// field the wire emitter now populates from `SessionConfig` +
/// `TranscribeResult` + the profile matcher.
///
/// The tests_support session default omits many of these (bare
/// SessionConfig), so we build a hand-populated config to prove the
/// wire emitter fans it out. Reload sinks / env overlay are covered by
/// their own unit tests in `metrics_sink::tests`.
#[test]
fn metrics_row_carries_full_utterance_schema() {
    use crate::dictate::session::SessionConfig;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.jsonl");

    let transcribe = TestTranscribe::returning_text("hej verden");
    let inject = TestInject::new();
    let config = SessionConfig {
        stt_backend: "whisper".to_owned(),
        model: "large-v3-turbo".to_owned(),
        device: "cuda".to_owned(),
        compute_type: "int8_float16".to_owned(),
        inject_mode: "auto".to_owned(),
        ..SessionConfig::default()
    };
    let (mut s, mut buf, _guard) =
        crate::dictate::session::tests_support::session_with_config(transcribe, inject, config);
    s = s.with_metrics_sink(Box::new(JsonlMetricsSink::new(path.clone())));

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let _ = s.stop_and_transcribe(&mut buf).expect("stop");

    let raw = fs::read_to_string(&path).unwrap();
    let row: Value = serde_json::from_str(raw.trim()).unwrap();

    // Python `_utterance_event` field list -- pinned here so a future
    // wire.rs refactor cannot silently drop a field.
    for (key, why) in [
        ("stt_backend", "session config"),
        ("model", "session config"),
        ("device", "session config"),
        ("compute_type", "session config"),
        ("inject_mode", "session config"),
        (
            "raw_text",
            "transcribe result (or dictionary_text fallback)",
        ),
        ("text_preview", "_compact_text of final text"),
        ("dictionary_text", "post-dictionary, pre-postprocess"),
        ("real_time_factor", "compute_s / audio_duration_s"),
    ] {
        assert!(
            row.get(key).is_some(),
            "metrics row is missing `{key}` ({why}); #606  4"
        );
    }
    assert_eq!(row["stt_backend"], "whisper");
    assert_eq!(row["model"], "large-v3-turbo");
    assert_eq!(row["device"], "cuda");
    assert_eq!(row["compute_type"], "int8_float16");
    assert_eq!(row["inject_mode"], "auto");
    // The test transcribe returns text "hej verden" without a raw_text
    // (the tests_support fixture uses `..Default::default()`), so the
    // session's fallback kicks in and raw_text mirrors the dictionary
    // text (which itself mirrors the final text when the dictionary is
    // a passthrough).
    assert_eq!(row["raw_text"], "hej verden");
    assert_eq!(row["dictionary_text"], "hej verden");
    assert_eq!(row["text_preview"], "hej verden");
    // real_time_factor = compute_s / audio_duration_s
    //                  = (42 ms / 1000) / 1.23 s
    //                  ≈ 0.03
    assert!(
        row["real_time_factor"].is_number(),
        "real_time_factor must be numeric"
    );
}

/// A broken metrics file (path whose parent cannot be created because the
/// parent is a regular file) must not abort the utterance -- the session
/// completes with `Injected`, the sink swallows the write error. Python
/// parity: `_record_utterance_event` wraps `append_record_sinks` in
/// `try / except OSError`.
#[test]
fn broken_metrics_path_does_not_abort_utterance() {
    let dir = tempfile::tempdir().unwrap();
    let file_as_parent = dir.path().join("not-a-dir");
    fs::write(&file_as_parent, "").unwrap();
    let path = file_as_parent.join("metrics.jsonl");

    let transcribe = TestTranscribe::returning_text("keep going");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_metrics_sink(Box::new(JsonlMetricsSink::new(path.clone())));

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");

    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "broken metrics file must not abort the utterance: {outcome:?}"
    );
    assert!(!path.exists(), "the unwritable path must stay absent");
}
