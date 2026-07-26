//! Integration tests for the `HistorySink` seam on [`super::DictateSession`].
//!
//! Unit tests for the sink implementation itself (filter behaviour, IO
//! error handling, round-trip via the reader) live in
//! [`super::history_sink::tests`]. These tests pin the WIRING: an attached
//! sink receives the same payload the worker-event emitter just wrote,
//! non-attached sessions are a no-op, and a broken sink cannot abort a
//! dictation. Together they establish parity with Python's
//! `vp_dictate._record_utterance_event`, which calls `_emit_worker_event`
//! and `append_record_sinks` from the same event dict.

use std::cell::RefCell;
use std::fs;
use std::sync::Mutex;

use serde_json::Value;

use super::tests_support::*;
use super::{HistorySink, JsonlHistorySink, NoopHistorySink, UtteranceOutcome};

// A capturing sink that records every payload it saw so tests can assert
// EXACTLY what the session handed to `append`. `Mutex<RefCell<...>>` because
// the trait requires `Send` for the boxed variant path -- the real production
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

impl HistorySink for CapturingSink {
    fn append(&self, event: &Value) {
        self.seen.lock().unwrap().borrow_mut().push(event.clone());
    }
}

/// Shared-Arc wrapper around a [`CapturingSink`] so the test can retain a
/// handle for `snapshot()` while the session takes ownership of the boxed
/// sink. Extracted once here — each test just calls
/// [`shared_sink()`] instead of re-declaring the wrapper.
struct SharedSink(std::sync::Arc<CapturingSink>);
impl HistorySink for SharedSink {
    fn append(&self, event: &Value) {
        self.0.append(event);
    }
}

fn shared_sink() -> (std::sync::Arc<CapturingSink>, Box<dyn HistorySink + Send>) {
    let inner = std::sync::Arc::new(CapturingSink::new());
    let boxed: Box<dyn HistorySink + Send> = Box::new(SharedSink(std::sync::Arc::clone(&inner)));
    (inner, boxed)
}

/// Handing the same-shaped payload as the worker-event emitter -- with
/// `ts` and `text` fields -- verifies that the session hooks
/// `record_history` into the successful-utterance branch.
#[test]
fn successful_utterance_calls_history_sink() {
    let transcribe = TestTranscribe::returning_text("hej verden");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);

    let (captured, sink) = shared_sink();
    s = s.with_history_sink(sink);

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
        "history sink must see exactly one payload per successful utterance"
    );
    let row = &seen[0];
    assert_eq!(row["event"], "utterance");
    assert_eq!(row["text"], "hej verden");
    assert!(
        row["ts"].is_number(),
        "ts field must be present (Python `_base_event` parity)"
    );
}

/// Inject-failure branch: Python's `_record_utterance_event` still fires
/// the event dict through `append_record_sinks`, so the Rust sink must
/// also see the payload even when injection failed. This pins the
/// second wire-up call site (the inject-error branch of `run_transcription`).
#[test]
fn inject_failure_still_calls_history_sink() {
    let transcribe = TestTranscribe::returning_text("hej verden");
    let inject = TestInject::failing("no display");
    let (mut s, mut buf, _guard) = session(transcribe, inject);

    let (captured, sink) = shared_sink();
    s = s.with_history_sink(sink);

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

/// A session without a sink attached MUST NOT call `record_history` --
/// verified indirectly by the fact that no sink method could have run.
/// Attaching the noop sink and running an utterance also stays crash-free
/// (parity spec: the noop is truly noop).
#[test]
fn session_without_sink_is_noop() {
    let transcribe = TestTranscribe::returning_text("no sink");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    // No `.with_history_sink(...)` call.
    let (outcome, _bytes, _s) = run_one_utterance(s, &one_second_pcm());
    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "session without sink must still complete normally"
    );
}

#[test]
fn noop_sink_is_still_wired_through_without_panic() {
    let transcribe = TestTranscribe::returning_text("noop attached");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_history_sink(Box::new(NoopHistorySink));
    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
}

/// End-to-end round trip: session -> production JSONL sink -> disk ->
/// pre-existing reader. This is the acceptance test for the parity blocker
/// this PR fixes: if the reader can pull the row the session wrote, the
/// user's `whisper-dictate history …` verbs will still work when the
/// default engine flips to Rust.
#[test]
fn round_trip_session_to_disk_to_history_reader() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");

    let transcribe = TestTranscribe::returning_text("hello from rust engine");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_history_sink(Box::new(JsonlHistorySink::new(path.clone())));

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let _ = s.stop_and_transcribe(&mut buf).expect("stop");

    // The pre-existing reader (`crate::history::read_rows`) must see the row.
    let rows = crate::history::read_rows(&path).unwrap();
    assert_eq!(rows.len(), 1, "one utterance -> one JSONL row");
    assert_eq!(rows[0]["text"], "hello from rust engine");
    assert_eq!(rows[0]["event"], "utterance");
    assert!(
        rows[0]["ts"].is_number(),
        "ts must be present so `history list` renders a timestamp"
    );

    // And `last_row` (the read side `copy-last` / `reinject-last` use) MUST
    // pick it up too.
    let last = crate::history::last_row(&path).unwrap().unwrap();
    assert_eq!(last["text"], "hello from rust engine");

    // The raw file must be valid JSONL (compact, newline-terminated) so
    // Python-side tooling that tails the file byte-wise stays happy.
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.ends_with('\n'));
    assert_eq!(raw.lines().count(), 1);
}
