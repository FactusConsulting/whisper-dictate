//! End-to-end coordinator integration tests for
//! [`super::rust_session_sink`]. Split out of `rust_session_sink_tests.rs`
//! so neither file exceeds the project's ~500-LOC modularity guideline
//! (AGENTS.md "Review guidelines", Codex P2 PR #421
//! rust_session_sink_coverage_tests.rs:4).
//!
//! Covers the synthetic Press / Release / Cancel events flowing through
//! the coordinator into the session, exercising the full
//! `start → push_frame → stop_and_transcribe → processing_finished`
//! lifecycle that mirrors `vp_dictate.py`'s `_processing_finished`
//! semantics. Unit-level tests for the pure helpers + the
//! `EventForwarder` framing live in the sibling
//! `rust_session_sink_tests.rs`; coverage-uplift tests live in
//! `rust_session_sink_coverage_tests.rs`.

use super::rust_session_sink::{build_session_action_sink, make_session, StubSession};
use super::test_support::EnvVarGuard;
use crate::hotkey::coordinator::{
    spawn as spawn_coordinator, CoordinatorEvent, CoordinatorHandle, CoordinatorThread, Mode,
    Options,
};
use crate::runtime::RuntimeEvent;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// RAII bundle the e2e fixture returns so each test holds the
/// crate-wide env lock AND restores `VOICEPI_WORKER_EVENTS` on exit.
/// Field order matters: the env-var guard drops BEFORE the lock guard
/// so the next test acquiring the lock observes the pre-fixture
/// worker-events value (Codex P2 PR #421 rust_session_sink_coverage_tests.rs).
struct EndToEndFixture {
    _worker_events_guard: EnvVarGuard,
    _env_lock_guard: MutexGuard<'static, ()>,
}

/// Acquire the crate-wide env lock and enable `VOICEPI_WORKER_EVENTS=1`
/// via a drop-restoring guard. Pulled out of the per-test bodies so the
/// boilerplate doesn't repeat across the three end-to-end tests below
/// and so the env-var leak fix lands in one place (Sonar CPD was also
/// flagging the duplication).
fn e2e_fixture() -> EndToEndFixture {
    let env_lock_guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let worker_events_guard = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    EndToEndFixture {
        _worker_events_guard: worker_events_guard,
        _env_lock_guard: env_lock_guard,
    }
}

/// Drain all events from `rx` into a `Vec` until it is empty or the
/// short timeout fires. Used after the coordinator + session have
/// processed a synthetic press/release pair.
fn drain_events(rx: &mpsc::Receiver<RuntimeEvent>) -> Vec<RuntimeEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
        out.push(ev);
    }
    out
}

/// Bundle of test handles returned by [`wire_coordinator_with_session`].
/// Factored into a struct rather than a 5-tuple so clippy's
/// `type_complexity` lint stays quiet (the tuple form was rejected by
/// CI's `clippy -- -D warnings` pass).
struct CoordinatorTestRig {
    coord: CoordinatorHandle,
    coord_thread: CoordinatorThread,
    session: Arc<Mutex<StubSession>>,
    rx: mpsc::Receiver<RuntimeEvent>,
    /// Every `processing_finished` id the sink emitted, in order.
    signaled: Arc<Mutex<Vec<u64>>>,
}

/// Build the rust-session sink, plug it into a fresh coordinator,
/// and return the wiring the test will exercise.
fn wire_coordinator_with_session(mode: Mode) -> CoordinatorTestRig {
    let (tx, rx) = mpsc::channel();
    let session = make_session();
    // Capture every `processing_finished` id the sink emits.
    let signaled: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));

    // `OnceLock` plays the same role here as in `build_production_sink`:
    // the closure is constructed BEFORE the coordinator (and thus the
    // CoordinatorHandle) exists, so the test pours the handle in after
    // `spawn_coordinator` returns. Without this two-step we would need
    // the channel sender exposed via a back-door from the coordinator,
    // which is exactly what the OnceLock-via-accessor pattern avoids.
    let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
    let coord_slot_for_signal = Arc::clone(&coord_slot);
    let signaled_for_sink = Arc::clone(&signaled);

    let sink = build_session_action_sink(
        Arc::clone(&session),
        tx,
        move |id| {
            signaled_for_sink.lock().unwrap().push(id);
            if let Some(handle) = coord_slot_for_signal.get() {
                handle.send(CoordinatorEvent::ProcessingFinished(id));
            }
        },
        None,
    );

    let (coord_handle, coord_thread) = spawn_coordinator(Options { mode }, sink, Instant::now);
    // `OnceLock::set` returns `Err(value)` on second call; we own the
    // slot here so this is the first writer.
    if coord_slot.set(coord_handle.clone()).is_err() {
        panic!("coord_slot must be empty on first set");
    }

    CoordinatorTestRig {
        coord: coord_handle,
        coord_thread,
        session,
        rx,
        signaled,
    }
}

/// Block until a worker event with the given state arrives or panic
/// after a short timeout. Worker events the test does not care about
/// (state mismatch) are still drained off the channel so subsequent
/// calls do not see them as the target.
fn wait_for_state(rx: &mpsc::Receiver<RuntimeEvent>, target: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(RuntimeEvent::Worker(w)) => {
                if w.state.as_deref() == Some(target) {
                    return;
                }
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    panic!("timed out waiting for state={target}");
}

/// Headline integration test for PR 4: a synthetic Press / Release
/// pair through the coordinator drives the session through its full
/// lifecycle and the `processing_finished` callback fires with the
/// matching id, leaving the session back in `Idle`.
#[test]
fn coordinator_press_release_drives_session_end_to_end() {
    let _fixture = e2e_fixture();

    let CoordinatorTestRig {
        coord,
        coord_thread,
        session,
        rx,
        signaled,
    } = wire_coordinator_with_session(Mode::HoldToTalk);

    // 1) Press → coordinator emits StartRecording → sink calls
    //    `session.start()`. Wait for the matching `state=recording`
    //    event so the next step pushes frames into a live recording.
    coord.send(CoordinatorEvent::Press);
    wait_for_state(&rx, "recording");

    // 2) Push one second of silent PCM directly into the session
    //    (PR 5 wires this through audio_route -- PR 4 deliberately
    //    leaves the audio side unwired so this PR can be tested
    //    standalone). Locking is safe: the sink only holds the session
    //    lock while a CoordinatorAction is being processed, and we
    //    just waited for the recording event so the sink is idle
    //    between events.
    {
        let mut sess = session.lock().unwrap();
        assert_eq!(
            sess.state(),
            crate::dictate::SessionState::Recording { id: 1 },
            "session must be Recording after coordinator Press"
        );
        let pcm = vec![0.0_f32; crate::dictate::session::SR as usize];
        sess.push_frame(&pcm);
    }

    // 3) Release → coordinator emits StopAndTranscribe → sink calls
    //    `session.stop_and_transcribe()`, the stub backend returns
    //    empty text with the stub gate string, the session emits
    //    `state=no_text reason=empty` + `state=ready`, the sink fires
    //    `processing_finished(1)`.
    coord.send(CoordinatorEvent::Release);
    wait_for_state(&rx, "ready");

    // Drain whatever stragglers landed before/after the ready.
    let events = drain_events(&rx);

    // 4) Settle: shut the coordinator down so the join() below doesn't
    //    hang. Tests must not leak threads -- the coordinator spawn()
    //    inside wire_coordinator_with_session runs an
    //    `mpsc::recv_timeout` loop that only exits on Shutdown.
    coord.shutdown();
    coord_thread.join();

    // ── assertions ─────────────────────────────────────────

    // The session must be back in Idle (Processing → ProcessingFinished
    // → Idle transition completed). The coordinator side mirrors this:
    // see the coordinator state machine in `hotkey/coordinator/mod.rs`.
    assert_eq!(
        session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle,
        "session must settle back to Idle after stop"
    );

    // The `processing_finished` callback fired exactly once with the
    // matching id. This is what unblocks the coordinator's
    // `Stage::Processing` guard so the NEXT press would be acted on
    // -- without it the coordinator would silently drop every press
    // after the first.
    let ids = signaled.lock().unwrap().clone();
    assert_eq!(ids, vec![1], "processing_finished must fire with id=1");

    // After `wait_for_state(ready)` returned, anything still in the
    // channel must NOT contain another "recording" or "ready" --
    // those are one-shot per utterance. The sequence between
    // `state=recording` (already consumed) and `state=ready` (already
    // consumed) is: transcribing → no_text → ready. We've already
    // taken those out of the channel via `wait_for_state`, so what
    // remains in `events` is empty in the happy case; the
    // `wait_for_state` calls themselves prove `recording` and `ready`
    // were emitted.
    let observed_states: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            RuntimeEvent::Worker(w) => w.state.clone(),
            _ => None,
        })
        .collect();
    assert!(
        !observed_states.iter().any(|s| s == "recording"),
        "should not see a duplicate `recording` event after wait_for_state"
    );
}

/// Variant of the above that asserts the full state sequence in
/// emission order by reading every worker event into a buffer
/// without filtering for a target state. Complements the headline
/// test by pinning the EXACT ordering the sink + session produce.
#[test]
fn coordinator_press_release_emits_full_state_sequence() {
    let _fixture = e2e_fixture();

    let CoordinatorTestRig {
        coord,
        coord_thread,
        session: _session,
        rx,
        signaled,
    } = wire_coordinator_with_session(Mode::HoldToTalk);

    coord.send(CoordinatorEvent::Press);
    coord.send(CoordinatorEvent::Release);

    // Collect at least `opening`, `recording`, `transcribing`,
    // `no_text`, `ready` -- the empty-buffer path (no frames pushed
    // before Release) emits `no_audio` rather than the stub-gated
    // `empty`. Both are acceptable proof the wire-up reached
    // `stop_and_transcribe`; we assert the SHAPE not the exact
    // reason here.
    let mut states = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(RuntimeEvent::Worker(w)) => {
                if let Some(state) = w.state {
                    let is_ready = state == "ready";
                    states.push(state);
                    if is_ready {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(_) => continue,
        }
    }

    coord.shutdown();
    coord_thread.join();

    assert_eq!(
        states,
        vec!["opening", "recording", "transcribing", "no_text", "ready"],
        "full Recording→Transcribing→Idle lifecycle"
    );
    assert_eq!(
        *signaled.lock().unwrap(),
        vec![1],
        "processing_finished fired once with the start id"
    );
}

/// PR 4 must mirror Cancel through to the session so a held-key release
/// that races a foreign chord drops the audio rather than transcribing
/// it. Mirrors the Python `_cancel_and_discard` path.
#[test]
fn coordinator_cancel_drives_session_cancel() {
    let _fixture = e2e_fixture();

    let CoordinatorTestRig {
        coord,
        coord_thread,
        session,
        rx,
        signaled,
    } = wire_coordinator_with_session(Mode::HoldToTalk);

    coord.send(CoordinatorEvent::Press);
    wait_for_state(&rx, "recording");
    coord.send(CoordinatorEvent::Cancel);
    wait_for_state(&rx, "ready"); // cancel emits cancelled then ready

    coord.shutdown();
    coord_thread.join();

    assert_eq!(
        session.lock().unwrap().state(),
        crate::dictate::SessionState::Idle,
        "session must settle to Idle after cancel"
    );
    assert!(
        signaled.lock().unwrap().is_empty(),
        "cancel must NOT trigger processing_finished -- the coordinator \
         drops straight to Idle without entering Stage::Processing"
    );
}
