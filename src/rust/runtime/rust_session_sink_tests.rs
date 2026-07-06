//! Tests for [`super::rust_session_sink`]. Split out of
//! `rust_session_sink.rs` so neither file exceeds the project's
//! ~500-LOC-per-file modularity guideline (AGENTS.md "Review
//! guidelines").
//!
//! Covers:
//!
//! - **Env gate** (`dictate_backend_rust_session_requested`): case
//!   insensitivity, whitespace trim, rejected values.
//! - **Wire framing** (`EventForwarder`, `parse_or_stderr`): partial
//!   writes get buffered; trailing partial lines surface on drop;
//!   `[worker-event] …` lines route to [`RuntimeEvent::Worker`];
//!   malformed payloads fall back to [`RuntimeEvent::Stderr`].
//!
//! The synthetic Press / Release / Cancel end-to-end tests that wire
//! the sink into a real coordinator live in
//! `rust_session_sink_e2e_tests.rs` (split out for the 500-LOC
//! modularity guideline, Codex P2 PR #421
//! rust_session_sink_coverage_tests.rs:4). Coverage-uplift tests live
//! in `rust_session_sink_coverage_tests.rs`.

use super::rust_session_sink::{
    build_production_sink, dictate_backend_rust_session_requested, parse_or_stderr, EventForwarder,
    StubInject, StubTranscribe, DICTATE_BACKEND_ENV, STUB_GATE_STRING, WORKER_EVENT_PREFIX,
};
use crate::dictate::{InjectBackend, TranscribeBackend};
use crate::runtime::RuntimeEvent;
use std::io::Write;
use std::sync::mpsc;
use std::sync::Arc;

// ── pure-logic helpers ────────────────────────────────────────────────────────

#[test]
fn dictate_backend_gate_reads_env_var_case_insensitive() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(DICTATE_BACKEND_ENV).ok();

    std::env::remove_var(DICTATE_BACKEND_ENV);
    assert!(!dictate_backend_rust_session_requested());

    std::env::set_var(DICTATE_BACKEND_ENV, "rust-session");
    assert!(dictate_backend_rust_session_requested());

    std::env::set_var(DICTATE_BACKEND_ENV, "RUST-SESSION");
    assert!(dictate_backend_rust_session_requested());

    std::env::set_var(DICTATE_BACKEND_ENV, "  rust-session  ");
    assert!(dictate_backend_rust_session_requested());

    std::env::set_var(DICTATE_BACKEND_ENV, "rust");
    assert!(!dictate_backend_rust_session_requested());

    std::env::set_var(DICTATE_BACKEND_ENV, "python");
    assert!(!dictate_backend_rust_session_requested());

    std::env::set_var(DICTATE_BACKEND_ENV, "");
    assert!(!dictate_backend_rust_session_requested());

    match prev {
        Some(v) => std::env::set_var(DICTATE_BACKEND_ENV, v),
        None => std::env::remove_var(DICTATE_BACKEND_ENV),
    }
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

// ── stub backends ─────────────────────────────────────────────────────────────

#[test]
fn stub_transcribe_returns_empty_text_with_stub_gate() {
    let stub = StubTranscribe;
    let result = stub
        .transcribe(&[0.0_f32; 16_000], 16_000)
        .expect("stub never errors");
    assert!(
        result.text.is_empty(),
        "stub transcribe must return empty text so the session takes the no_text path"
    );
    assert_eq!(result.gate.as_deref(), Some(STUB_GATE_STRING));
}

#[test]
fn stub_inject_is_a_noop_that_always_succeeds() {
    let stub = StubInject;
    // The PR 4 stub backend always succeeds; PR 5 swaps for the real
    // injection dispatcher that surfaces enigo/ydotool failures.
    assert!(stub.inject("any text").is_ok());
    assert!(
        stub.inject("").is_ok(),
        "empty text path must also succeed -- the session calls inject \
         only on the non-empty branch today but the stub must be robust"
    );
}

// ── production sink wiring ────────────────────────────────────────────────────

#[test]
fn build_production_sink_returns_empty_coordinator_slot() {
    // `build_production_sink` constructs the closure BEFORE the
    // coordinator exists; the supervisor pours the live
    // CoordinatorHandle into the returned `OnceLock` after
    // `install_hotkey` succeeds. This test pins the contract: the
    // slot must come back empty so the supervisor can populate it
    // without losing the existing value. Holds for BOTH PR 4 stubs
    // AND PR 5 real backends.
    //
    // Codex P2 #416 (round 2) rust_session_sink_tests.rs:143 --
    // `build_production_sink` mutates the process-wide
    // `VOICEPI_WORKER_EVENTS` env var to enable the in-process gate
    // (and, on the real-backend path, may read
    // `VOICEPI_WHISPER_MODEL_PATH`). Take the crate-wide ENV_LOCK so
    // this does not race against `dictate::events_tests::*`, which
    // toggles the same var while asserting the gate suppresses
    // output; and restore the prior value on exit so a
    // `--test-threads=1` run leaves the env untouched.
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(crate::dictate::events::WORKER_EVENTS_ENV).ok();

    let (tx, _rx) = mpsc::channel();
    let (_sink, coord_slot) = build_production_sink(tx, None);
    assert!(
        coord_slot.get().is_none(),
        "production sink must hand back an empty OnceLock for the supervisor to populate"
    );

    match prev {
        Some(v) => std::env::set_var(crate::dictate::events::WORKER_EVENTS_ENV, v),
        None => std::env::remove_var(crate::dictate::events::WORKER_EVENTS_ENV),
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

/// Pins Codex P2 #416 rust_session_sink.rs:289 fix: the repaint
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
