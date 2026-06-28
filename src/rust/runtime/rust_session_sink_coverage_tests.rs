//! Coverage uplift tests for [`super::rust_session_sink`]. Split out of
//! `rust_session_sink_tests.rs` so neither file exceeds the project's
//! ~500-LOC-per-file modularity guideline (AGENTS.md "Review
//! guidelines").
//!
//! Targets the Sonar new-code coverage gate on PR #416 -- the original
//! tests in the sibling module landed coverage at 79.0% (gate at 80%);
//! these tests pick up the still-uncovered branches:
//!
//! - `parse_or_stderr` fallback when the JSON `event` field is missing,
//!   non-string, or the `state` field is absent (sink.rs L383).
//! - `EventForwarder::drop` invokes the repaint notifier on the
//!   trailing-line flush path (sink.rs L365). Pairs with the per-newline
//!   `event_forwarder_invokes_repaint_notifier_after_each_event` test.
//! - Sink start-failure error formatting (sink.rs L181-L183) -- triggered
//!   by a duplicate `StartRecording` action.
//! - Sink stop/cancel happy-path semantics that share the L189-L227
//!   action-matcher block with the (untestable-from-here) writer-error
//!   branches: stop-from-idle returns Ok(NotRecording), cancel-from-idle
//!   no-ops, stale-epoch cancel during recording is rejected by the
//!   session's epoch guard. These ensure the matcher branches are
//!   executed even though the error-format lines stay uncovered
//!   (they require a failing `Write` impl, which EventForwarder isn't).
//! - `build_production_sink`'s `processing_finished` closure tolerates
//!   an empty coordinator-slot -- mirrors the lazy-population gap.

use super::rust_session_sink::{
    build_production_sink, build_session_action_sink, make_session, parse_or_stderr,
    EventForwarder, WORKER_EVENT_PREFIX,
};
use crate::runtime::RuntimeEvent;
use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

// ── parse_or_stderr fallback branches (Sonar L383) ────────────────────

/// Pins Sonar L383: `parse_or_stderr` returns Stderr when the JSON
/// payload parses but lacks the required `event` string field.
#[test]
fn parse_or_stderr_falls_back_when_event_field_absent() {
    let line = format!("{WORKER_EVENT_PREFIX}{{\"state\":\"recording\"}}");
    match parse_or_stderr(line.clone()) {
        RuntimeEvent::Stderr(s) => assert_eq!(s, line),
        other => panic!("expected Stderr fallback when `event` field missing, got {other:?}"),
    }
}

/// Pins Sonar L383 sibling: payload with a non-string `event` field.
/// `value.as_str()` returns None for numbers, triggering the same
/// fallback branch.
#[test]
fn parse_or_stderr_falls_back_when_event_field_is_not_a_string() {
    let line = format!("{WORKER_EVENT_PREFIX}{{\"event\":42}}");
    match parse_or_stderr(line.clone()) {
        RuntimeEvent::Stderr(s) => assert_eq!(s, line),
        other => panic!("expected Stderr fallback for non-string event, got {other:?}"),
    }
}

/// Pins the `state`-extraction branch in `parse_or_stderr`: a worker
/// event whose payload omits `state` maps to `WorkerEvent::state = None`
/// rather than failing.
#[test]
fn parse_or_stderr_returns_worker_with_no_state_when_omitted() {
    let line = format!("{WORKER_EVENT_PREFIX}{{\"event\":\"heartbeat\"}}");
    match parse_or_stderr(line) {
        RuntimeEvent::Worker(w) => {
            assert_eq!(w.event, "heartbeat");
            assert!(
                w.state.is_none(),
                "missing state field -> WorkerEvent::state = None"
            );
        }
        other => panic!("expected Worker, got {other:?}"),
    }
}

// ── EventForwarder::drop repaint-notifier branch (Sonar L365) ─────────

/// Pins Sonar L365: `EventForwarder::drop` invokes the repaint notifier
/// when a partial (no-newline) line is flushed as Stderr on drop.
/// The sibling test in `rust_session_sink_tests` covers the per-newline
/// notifier call; this one covers the trailing-flush branch in `Drop`.
#[test]
fn event_forwarder_invokes_repaint_notifier_on_drop_flush() {
    let (tx, rx) = mpsc::channel();
    let wakeups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wakeups_for_notifier = Arc::clone(&wakeups);
    let notifier: crate::runtime::RepaintNotifier = Arc::new(move || {
        wakeups_for_notifier.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    {
        let mut fwd = EventForwarder::new(&tx, Some(&notifier));
        // No trailing newline -- the line stays buffered until Drop,
        // which is the path that runs the Drop-side notifier call.
        fwd.write_all(b"partial-line-no-newline").unwrap();
    }
    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        events.len(),
        1,
        "Drop flushed the buffered line as one event"
    );
    match &events[0] {
        RuntimeEvent::Stderr(s) => assert_eq!(s, "partial-line-no-newline"),
        other => panic!("expected Stderr from Drop flush, got {other:?}"),
    }
    assert_eq!(
        wakeups.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Drop-time flush must invoke the repaint notifier once"
    );
}

// ── sink action-matcher branches (Sonar L181-L221, partial) ───────────

/// Pins Sonar L181-L183: the sink surfaces a `session.start()` failure
/// as `RuntimeEvent::Error` with the documented prefix. Exercised by a
/// double `StartRecording` action -- the session returns
/// `SessionError::AlreadyActive` on the second one.
#[test]
fn sink_forwards_session_start_failure_as_runtime_error() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

    let (tx, rx) = mpsc::channel();
    let session = make_session();
    let signaled = Arc::new(Mutex::new(Vec::<u64>::new()));
    let signaled_for_sink = Arc::clone(&signaled);
    let mut sink = build_session_action_sink(
        Arc::clone(&session),
        tx,
        move |id| signaled_for_sink.lock().unwrap().push(id),
        None,
    );

    use crate::hotkey::coordinator::CoordinatorAction;
    sink(CoordinatorAction::StartRecording(1));
    // Second start: session is already Recording -> the sink hits the
    // start-failed error formatter (Sonar L181-L183).
    sink(CoordinatorAction::StartRecording(2));

    let events: Vec<_> = rx.try_iter().collect();
    let saw_error = events.iter().any(|ev| {
        matches!(
            ev,
            RuntimeEvent::Error(msg) if msg.starts_with("[rust-session] start failed (coord id=2)")
        )
    });
    assert!(
        saw_error,
        "double-start must surface `[rust-session] start failed` error: {events:?}"
    );
    assert!(
        signaled.lock().unwrap().is_empty(),
        "no processing_finished should fire on a start failure"
    );
}

/// Covers the `StopAndTranscribe` matcher arm (sink.rs L189-L207) when
/// the session returns `Ok(UtteranceOutcome::NotRecording)` -- i.e. the
/// no-op path the Python wrapper mirrors. The error-format branch
/// (L198-L200) only fires on a `Write` failure, which the
/// `EventForwarder` writer never produces, so we can't exercise that
/// branch from a unit test; this test asserts the happy semantics
/// instead (no Error event, processing_finished still fires to unblock
/// the coordinator).
#[test]
fn sink_stop_from_idle_is_noop_but_signals_processing_finished() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

    let (tx, rx) = mpsc::channel();
    let session = make_session();
    let signaled = Arc::new(Mutex::new(Vec::<u64>::new()));
    let signaled_for_sink = Arc::clone(&signaled);
    let mut sink = build_session_action_sink(
        Arc::clone(&session),
        tx,
        move |id| signaled_for_sink.lock().unwrap().push(id),
        None,
    );

    use crate::hotkey::coordinator::CoordinatorAction;
    sink(CoordinatorAction::StopAndTranscribe(7));

    let events: Vec<_> = rx.try_iter().collect();
    let saw_error = events.iter().any(|ev| {
        matches!(
            ev,
            RuntimeEvent::Error(msg) if msg.starts_with("[rust-session] stop failed")
        )
    });
    assert!(
        !saw_error,
        "stop-from-idle returns Ok(NotRecording); the sink must NOT emit an Error event: {events:?}"
    );
    assert_eq!(
        *signaled.lock().unwrap(),
        vec![7],
        "processing_finished MUST fire even on the no-op stop branch so the \
         coordinator does not wedge in Stage::Processing -- mirrors Python's \
         `finally: _processing_finished` semantics"
    );
    assert_eq!(
        session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle,
        "session stays Idle after a no-op stop"
    );
}

/// Covers the `CancelRecording` matcher arm (sink.rs L208-L227) for the
/// stale-epoch-during-recording branch. The session's own epoch guard
/// silently no-ops the mismatched cancel; the sink must propagate that
/// no-op without panicking and without tearing the active recording
/// down.
#[test]
fn sink_cancel_with_mismatched_epoch_during_recording_is_safe() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

    let (tx, _rx) = mpsc::channel();
    let session = make_session();
    let mut sink = build_session_action_sink(Arc::clone(&session), tx, |_id| {}, None);

    use crate::hotkey::coordinator::CoordinatorAction;
    sink(CoordinatorAction::StartRecording(1));
    // Cancel with a non-matching epoch: session's own epoch guard
    // silently ignores; sink must not panic and recording must continue.
    sink(CoordinatorAction::CancelRecording(99));

    assert_eq!(
        session.lock().unwrap().state(),
        crate::dictate::SessionState::Recording { id: 1 },
        "stale-epoch cancel must NOT tear down an active recording"
    );

    // Settle: matching-epoch cancel clears state.
    sink(CoordinatorAction::CancelRecording(1));
    assert_eq!(
        session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle,
        "matching-epoch cancel must return session to Idle"
    );
}

/// Covers the `CancelRecording` matcher arm from the Idle state -- a
/// no-op for the session (`cancel()` returns Ok(()) for any
/// non-Recording/Opening state). Pairs with the test above to cover
/// both Idle-vs-Recording entry conditions to the cancel branch.
#[test]
fn sink_cancel_from_idle_is_a_safe_noop() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

    let (tx, rx) = mpsc::channel();
    let session = make_session();
    let mut sink = build_session_action_sink(Arc::clone(&session), tx, |_id| {}, None);

    use crate::hotkey::coordinator::CoordinatorAction;
    sink(CoordinatorAction::CancelRecording(99));

    let events: Vec<_> = rx.try_iter().collect();
    let saw_error = events.iter().any(|ev| {
        matches!(
            ev,
            RuntimeEvent::Error(msg) if msg.starts_with("[rust-session] cancel failed")
        )
    });
    assert!(
        !saw_error,
        "cancel-from-idle is Ok(()); the sink must NOT emit an Error event: {events:?}"
    );
    assert_eq!(
        session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle,
        "session stays Idle"
    );
}

// ── build_production_sink processing_finished closure (Sonar L276-L280)

/// Pins Sonar L276-L280: `build_production_sink`'s closure body for
/// `processing_finished` is a silent no-op when the coordinator slot
/// is empty (the window between sink construction and the supervisor
/// pouring the live `CoordinatorHandle` in via `OnceLock::set`).
#[test]
fn production_sink_processing_finished_is_noop_when_coord_slot_empty() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

    let (tx, _rx) = mpsc::channel();
    let (mut sink, coord_slot) = build_production_sink(tx, None);
    assert!(coord_slot.get().is_none(), "precondition: slot empty");

    use crate::hotkey::coordinator::CoordinatorAction;
    // Drive a stop directly -- the session is Idle so stop_and_transcribe
    // returns Ok(NotRecording); the sink then calls the
    // `processing_finished` closure body (Sonar L276-L280) which sees
    // `coord_slot_for_signal.get() = None` and silently no-ops.
    sink(CoordinatorAction::StopAndTranscribe(42));

    assert!(
        coord_slot.get().is_none(),
        "processing_finished must not retroactively populate the slot"
    );
}
