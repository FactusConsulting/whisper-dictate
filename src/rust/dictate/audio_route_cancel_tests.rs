//! Cancel-event handling tests for [`crate::dictate::audio_route::AudioRoute`].
//!
//! Split out of `audio_route_tests.rs` to keep both files under the
//! AGENTS.md ~500 LOC modularity bar (Codex P2 #415
//! audio_route_tests.rs:523). The two scenarios here both exercise the
//! same Phase-1 silent-drop policy for `PipelineEvent::Cancelled` --
//! one in-flight, one across a `start_recording` boundary -- so keeping
//! them in the same file groups the chord-race rationale; the shared
//! `audio_route_test_support` module supplies the backends + env guards.

use crate::audio::PipelineEvent;
use crate::dictate::audio_route_test_support::{route_with_cap, EnvVarGuard};
use crate::dictate::session::SessionState;
use crate::test_env_lock::ENV_LOCK;

/// `PipelineEvent::Cancelled` is currently dropped silently (Phase-1
/// parity with `vp_capture_rust_stdin.py:228-232`; the pipeline event
/// carries no recording id, so routing it through
/// `DictateSession::cancel` would race the chord-cancel epoch guard).
/// Pinned so a future change that wires Cancelled into session.cancel
/// has to also solve the epoch-race problem Codex flagged
/// (P2 #415 audio_route.rs:300).
#[test]
fn cancelled_event_dropped_silently_no_state_change() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    route.start_recording(&mut buf).expect("start_recording");
    let pre_state = route.session().state();
    let pre_epoch = route.session().epoch();
    buf.clear();

    // Push a couple of frames so the route has non-zero buffered
    // samples — Cancelled must NOT discard them either (the Python
    // path doesn't touch the buffer on Cancelled in Phase 1).
    for _ in 0..2 {
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
            .expect("Frame");
    }
    assert_eq!(route.buffered_samples(), 960);
    buf.clear();

    let marker = route
        .on_event(PipelineEvent::Cancelled, &mut buf)
        .expect("Cancelled");
    assert_eq!(marker, None, "Cancelled returns no SpeechMarker");
    assert_eq!(
        route.session().state(),
        pre_state,
        "Cancelled must NOT change session state in Phase 1",
    );
    assert_eq!(
        route.session().epoch(),
        pre_epoch,
        "Cancelled must NOT bump the recording epoch",
    );
    assert_eq!(
        route.buffered_samples(),
        960,
        "Cancelled must NOT discard buffered audio in Phase 1",
    );
    assert!(
        buf.is_empty(),
        "no worker events emitted on Cancelled drop; got: {buf:?}",
    );
}

/// P2-C explicit boundary scenario (Codex P2 #415 audio_route.rs:300)
/// -- a stale `Cancelled` queued from a prior pipeline reset must NOT
/// discard the new recording when it lands across a `start_recording`
/// boundary. The Phase-1 silent-drop policy makes this safe; without
/// it the route would call `session.cancel(session.epoch())` (which
/// always matches) and discard the new audio.
#[test]
fn stale_cancelled_across_start_boundary_preserves_new_recording() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();

    // Recording A: open + close so a stale Cancelled could be queued.
    let epoch_a = route.start_recording(&mut buf).expect("start A");
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("A frame");
    route.stop_recording(&mut buf).expect("stop A");
    buf.clear();

    // Recording B: fresh start, push frames, then receive A's stale Cancelled.
    let epoch_b = route.start_recording(&mut buf).expect("start B");
    assert_ne!(epoch_a, epoch_b, "epochs must differ across start boundary");
    for _ in 0..8 {
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
            .expect("B frame");
    }
    route
        .on_event(PipelineEvent::Cancelled, &mut buf)
        .expect("stale Cancelled must not error");

    // Load-bearing: B's epoch + buffered audio survive.
    assert_eq!(route.session().epoch(), epoch_b);
    assert_eq!(route.buffered_samples(), 3840);
    assert!(matches!(
        route.session().state(),
        SessionState::Recording { .. }
    ));

    route.stop_recording(&mut buf).expect("stop B");
    assert_eq!(route.session().state(), SessionState::Idle);
}
