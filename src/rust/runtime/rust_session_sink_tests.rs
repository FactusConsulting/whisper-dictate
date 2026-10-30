//! Tests for [`super::rust_session_sink`]. Split out of
//! `rust_session_sink.rs` so neither file exceeds the project's
//! guidelines").
//!
//! Covers:
//!
//! - **No stub fallback**: the production builder refuses when the real
//!   backends are not compiled in, and the module carries neither a
//!   lenient builder nor a blanket `dead_code` allow that could hide one.
//! - **Terminal panic boundary** (`terminal_panic_boundary`): a panicking
//!   action emits an error + exit event and disables later actions.
//! - **Wire framing** (`EventForwarder`, `parse_or_stderr`): partial
//!   writes get buffered; trailing partial lines surface on drop;
//!   `[worker-event] …` lines route to [`RuntimeEvent::Worker`];
//!   malformed payloads fall back to [`RuntimeEvent::Stderr`].
//!
//! The synthetic Press / Release / Cancel end-to-end tests that wire
//! the sink into a real coordinator live in
//! `rust_session_sink_e2e_tests.rs` (split out for the 500-LOC
//! modularity guideline; see
//! `rust_session_sink_coverage_tests.rs`). Coverage-uplift tests live
//! in `rust_session_sink_coverage_tests.rs`.

use super::rust_session_sink::{
    parse_or_stderr, terminal_panic_boundary, CoordinatorActionSink, EventForwarder,
    WORKER_EVENT_PREFIX,
};
use crate::hotkey::coordinator::CoordinatorAction;
use crate::runtime::RuntimeEvent;
use std::io::Write;
use std::sync::mpsc;
use std::sync::Arc;

// ── no stub fallback ──────────────────────────────────────────────────────────

/// Without `whisper-rs-local` the real backends do not exist, and the only
/// production builder must say so instead of installing a sink that
/// answers every PTT press with an empty transcription. Nothing may be
/// enqueued on `tx` either: a degraded sink used to announce itself with
/// a stderr event and then report ready.
#[cfg(all(
    feature = "rust-hotkeys",
    feature = "rust-injection",
    not(feature = "whisper-rs-local")
))]
#[test]
fn production_sink_refuses_without_real_backends() {
    let (tx, rx) = mpsc::channel();
    let result = super::rust_session_sink::try_build_production_sink(
        tx,
        None,
        super::live_settings::LiveEnvOverrides::default(),
        super::settings_snapshot::RuntimeSettingsSnapshot::default(),
    );
    let err = match result {
        Ok(_) => panic!("builder must refuse without whisper-rs-local"),
        Err(err) => err,
    };
    assert!(
        err.contains("whisper-rs-local") && err.contains("shipping"),
        "refusal must name the missing feature and the rebuild profile: {err}"
    );
    assert!(
        rx.try_recv().is_err(),
        "a refused build must not enqueue any runtime event"
    );
}

// ── pure-logic helpers ────────────────────────────────────────────────────────

#[test]
fn coordinator_panic_emits_terminal_exit_and_disables_later_actions() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_sink = Arc::clone(&calls);
    let inner: CoordinatorActionSink = Box::new(move |_action| {
        calls_for_sink.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("lazy model load failed");
    });
    let (tx, rx) = mpsc::channel();
    let mut sink = terminal_panic_boundary(inner, tx, None);

    sink(CoordinatorAction::StartRecording(41));
    sink(CoordinatorAction::StartRecording(42));

    let events: Vec<_> = rx.try_iter().collect();
    assert!(events.iter().any(
        |event| matches!(event, RuntimeEvent::Error(message) if message.contains("lazy model load failed"))
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Exited { code: Some(1) })));
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "terminal boundary must not call a failed action sink again"
    );
}

#[test]
fn parse_or_stderr_routes_worker_events() {
    let line = format!(
        "{}{}",
        WORKER_EVENT_PREFIX, r#"{"event":"status","state":"recording"}"#
    );
    match parse_or_stderr(line) {
        RuntimeEvent::Worker(w) => {
            assert_eq!(w.event, "status");
            assert_eq!(w.state.as_deref(), Some("recording"));
        }
        other => panic!("expected Worker, got {other:?}"),
    }
}

#[test]
fn parse_or_stderr_passes_other_lines_through_as_stderr() {
    let line = "plain log line".to_owned();
    match parse_or_stderr(line) {
        RuntimeEvent::Stderr(s) => assert_eq!(s, "plain log line"),
        other => panic!("expected Stderr, got {other:?}"),
    }
}

#[test]
fn parse_or_stderr_falls_back_to_stderr_on_malformed_payload() {
    let line = format!("{WORKER_EVENT_PREFIX}{{not-json");
    match parse_or_stderr(line.clone()) {
        RuntimeEvent::Stderr(s) => assert_eq!(s, line),
        other => panic!("expected Stderr fallback, got {other:?}"),
    }
}

// ── EventForwarder framing ────────────────────────────────────────────────────

#[test]
fn event_forwarder_buffers_partial_writes() {
    let (tx, rx) = mpsc::channel();
    {
        let mut fwd = EventForwarder::new(&tx, None);
        fwd.write_all(b"hello ").unwrap();
        fwd.write_all(b"world\n").unwrap();
    }
    let ev = rx.try_recv().expect("one line");
    match ev {
        RuntimeEvent::Stderr(s) => assert_eq!(s, "hello world"),
        other => panic!("expected Stderr, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "exactly one line");
}

#[test]
fn event_forwarder_drains_trailing_partial_line_on_drop() {
    let (tx, rx) = mpsc::channel();
    {
        let mut fwd = EventForwarder::new(&tx, None);
        fwd.write_all(b"no-newline").unwrap();
    }
    match rx.try_recv().expect("drained trailing line") {
        RuntimeEvent::Stderr(s) => assert_eq!(s, "no-newline"),
        other => panic!("expected Stderr trailing, got {other:?}"),
    }
}

/// Pins the contract: the repaint
/// notifier fires once per event the forwarder enqueues so the egui
/// UI wakes up to process it (the Windows minimised-window pattern
/// the supervisor's `repaint_notifier` doc comment describes).
#[test]
fn event_forwarder_invokes_repaint_notifier_after_each_event() {
    let (tx, rx) = mpsc::channel();
    let wakeups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wakeups_for_notifier = Arc::clone(&wakeups);
    let notifier: crate::runtime::RepaintNotifier = Arc::new(move || {
        wakeups_for_notifier.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    {
        let mut fwd = EventForwarder::new(&tx, Some(&notifier));
        fwd.write_all(b"line one\nline two\n").unwrap();
    }
    // Two complete lines -> two RuntimeEvents on tx -> two wakeups.
    assert_eq!(
        rx.try_iter().count(),
        2,
        "two complete lines must produce two RuntimeEvents"
    );
    assert_eq!(
        wakeups.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "repaint notifier must fire once per enqueued event"
    );
}
