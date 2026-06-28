//! Unit tests for [`crate::dictate::audio_route::AudioRoute`].
//!
//! Covers the four behaviour gates the route mirrors from
//! `src/python/whisper_dictate/vp_capture_rust_stdin.py` plus the
//! refinements from Codex P2 review on #415 (idle-marker drop,
//! cap-without-auto-stop, capture_lost status, env-refresh on start).
//!
//! The tests construct the route via `AudioRoute::for_test`, which
//! skips the real cpal pipeline — every `PipelineEvent` flows through
//! `on_event` directly, the same path the supervisor (PR 4) will use.
//! No cpal / Silero / Whisper dependency in the test binary. Shared
//! test backends + env helpers live in `audio_route_test_support.rs`
//! to keep this file under the 500 LOC bar.

use crate::audio::PipelineEvent;
use crate::dictate::audio_route::{RouteConfig, RouteError, SpeechMarker};
use crate::dictate::audio_route_test_support::{
    parse_events, route_with_cap, start_recording_with_cap_env, EnvVarGuard,
};
use crate::dictate::session::{SessionState, UtteranceOutcome, SR};
use crate::test_env_lock::ENV_LOCK;

// ── tests ────────────────────────────────────────────────────────────────────

/// Gate 1 — the first `Frame` after `start_recording()` lands in the
/// buffer. Guards against a regression where the route checks state
/// before the session has finished transitioning to Recording.
#[test]
fn first_frame_after_start_lands_in_session_buffer() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    // Disable the cap explicitly so the env-refresh in start_recording
    // doesn't default to 120 s (harmless for this test, but more
    // honest to have a single behavioural axis under test at a time).
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    route.start_recording(&mut buf).expect("start_recording");
    assert_eq!(route.buffered_samples(), 0, "buffer must reset on start");
    assert!(
        matches!(route.session().state(), SessionState::Recording { .. }),
        "session must be Recording after start",
    );

    let frame: Vec<f32> = vec![0.5; 480]; // one 30 ms / 480-sample frame at 16 kHz
    let marker = route
        .on_event(PipelineEvent::Frame(frame), &mut buf)
        .expect("on_event(Frame)");
    assert_eq!(marker, None, "Frame returns no SpeechMarker");
    assert_eq!(
        route.buffered_samples(),
        480,
        "first frame's samples must be counted, not dropped",
    );
}

/// Gate 2 — frames pushed while the session is idle (never
/// `start_recording`-ed) are dropped silently. No error, no events.
#[test]
fn frames_dropped_when_session_idle() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    assert_eq!(route.session().state(), SessionState::Idle);

    let frame: Vec<f32> = vec![0.5; 480];
    let marker = route
        .on_event(PipelineEvent::Frame(frame), &mut buf)
        .expect("idle Frame must NOT error — vp_capture_rust_stdin drops silently");
    assert_eq!(marker, None);
    assert_eq!(
        route.buffered_samples(),
        0,
        "buffer must stay empty when no recording is in flight",
    );
    assert!(
        buf.is_empty(),
        "no worker events emitted for an idle-drop, got: {buf:?}",
    );
}

/// Gate 3a — once the buffered duration exceeds the cap, the route
/// refuses further frames, emits a one-shot
/// `status=recording capped=true recording_s=N` worker event, AND
/// keeps the session in Recording until the supervisor's PTT-release
/// handler closes it. The recording is **not** auto-stopped mid-press:
/// auto-stopping would inject text while the PTT modifier is still
/// down, which for bare-modifier bindings (Ctrl, Alt) would trigger
/// keyboard shortcuts on the injected characters. Codex P2 #415
/// audio_route.rs:339.
///
/// Cap = 0.03 s = 480 samples (one frame). Push two frames: the second
/// trips the cap. Then push a third frame to assert it is also
/// dropped silently (no repeated `capped` event).
#[test]
fn max_record_cap_refuses_over_cap_frames_and_emits_capped_status() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    let _id = start_recording_with_cap_env(&mut route, &mut buf, 0.03);
    // Drop the start_recording events so the assertions below only
    // see what the on_event calls emit.
    buf.clear();

    // First frame fits (480 == cap).
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("first frame within cap");
    assert_eq!(route.buffered_samples(), 480);
    assert!(!route.cap_tripped(), "cap not yet tripped after one frame");

    // Second frame pushes to 960 samples = 0.06 s > 0.03 s cap → trip.
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("trip frame must not error");
    assert!(route.cap_tripped(), "cap_tripped after over-cap frame");
    assert_eq!(
        route.buffered_samples(),
        480,
        "over-cap frame must NOT be appended to the buffer",
    );
    assert!(
        matches!(route.session().state(), SessionState::Recording { .. }),
        "session must STAY Recording — supervisor closes it on PTT release",
    );

    // Third frame: also dropped, no new `capped` event (one-shot).
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("third frame must not error");
    assert_eq!(
        route.buffered_samples(),
        480,
        "subsequent over-cap frames stay dropped",
    );

    // Exactly one event landed: the capped status line.
    let events = parse_events(&buf);
    assert_eq!(
        events.len(),
        1,
        "exactly one capped event emitted on the first trip (no repeats); got: {events:?}",
    );
    let ev = &events[0];
    assert_eq!(ev["event"].as_str(), Some("status"));
    assert_eq!(ev["state"].as_str(), Some("recording"));
    assert_eq!(ev["capped"].as_bool(), Some(true));
    // 960 samples / 16000 Hz = 0.06 s → rounded to 1 dp = 0.1
    // (Python's `round(buffered_s, 1)` from vp_capture_rust_stdin.py:222).
    assert_eq!(ev["recording_s"].as_f64(), Some(0.1));
}

/// Companion to the cap test — after a cap-trip, the supervisor still
/// calls `stop_recording` on PTT release and the session closes
/// normally (frames already buffered transcribe; the over-cap frames
/// that were dropped don't).
#[test]
fn cap_tripped_recording_closes_normally_on_stop_recording() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    // Cap = 0.36 s = 5760 samples (above the 0.3 s misfire floor in
    // `crate::dictate::skip`, so the buffered audio reaches the
    // transcriber rather than the too-short skip branch).
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    let _id = start_recording_with_cap_env(&mut route, &mut buf, 0.36);

    // 12 frames = 5760 samples = exactly cap. 13th trips.
    for _ in 0..12 {
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
            .expect("under-cap frame");
    }
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("trip frame");
    assert!(route.cap_tripped());

    // Supervisor releases PTT.
    let outcome = route.stop_recording(&mut buf).expect("stop_recording");
    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "post-trip stop must transcribe the under-cap buffer; got {outcome:?}",
    );

    let pcm_lens: Vec<usize> = route
        .session()
        .transcribe_backend()
        .seen_pcm_len
        .borrow()
        .clone();
    assert_eq!(
        pcm_lens,
        vec![5760],
        "transcribe must receive exactly the under-cap buffer (no over-cap samples)",
    );
}

/// Gate 3b — `RouteConfig::from_env` mirrors `vp_capture._max_record_s`:
/// an unset OR unparseable variable falls back to the 120 s Python
/// default; an explicit non-positive value disables the cap; positive
/// finite values pass through.
#[test]
fn route_config_from_env_parses_max_record_seconds() {
    use crate::dictate::audio_route::DEFAULT_MAX_RECORD_S;
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    {
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "12.5");
        assert_eq!(RouteConfig::from_env().max_record_seconds, Some(12.5));
    }
    {
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
        assert_eq!(
            RouteConfig::from_env().max_record_seconds,
            None,
            "0 must disable the cap (matches vp_capture._max_record_s)",
        );
    }
    {
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "-5");
        assert_eq!(
            RouteConfig::from_env().max_record_seconds,
            None,
            "negative values must disable the cap",
        );
    }
    {
        // Python's `_max_record_s` does `float(raw)` inside try/except;
        // a parse failure falls back to the 120 s default rather than
        // disabling the cap. Match that.
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "not-a-number");
        assert_eq!(
            RouteConfig::from_env().max_record_seconds,
            Some(DEFAULT_MAX_RECORD_S),
            "unparseable values must fall back to the 120 s default",
        );
    }
    {
        // `(os.environ.get(...) or "120").strip()` — an unset variable
        // is the same code path as the unparseable fallback above:
        // both land on 120 s.
        let _e = EnvVarGuard::remove("VOICEPI_MAX_RECORD_S");
        assert_eq!(
            RouteConfig::from_env().max_record_seconds,
            Some(DEFAULT_MAX_RECORD_S),
            "unset env var must fall back to the 120 s Python default",
        );
    }
    {
        // Whitespace is stripped before parsing, matching Python's `.strip()`.
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "  42  ");
        assert_eq!(
            RouteConfig::from_env().max_record_seconds,
            Some(42.0),
            "leading/trailing whitespace must be ignored",
        );
    }
}

/// Gate 4 — a `PipelineEvent::DeviceError` emits a
/// `status=capture_lost` worker line (NOT an `event=error` line; the
/// Rust UI dispatcher only handles `event=status`/`audio`/`utterance`,
/// so an `event=error` line would be parsed and then dropped — see
/// Codex P2 #415 audio_route.rs:358) and returns `RouteError::Device`.
#[test]
fn device_error_emits_capture_lost_status_and_returns_route_error() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();

    let err = route
        .on_event(PipelineEvent::DeviceError("mic unplugged".into()), &mut buf)
        .expect_err("DeviceError must surface as RouteError");
    match err {
        RouteError::Device(msg) => assert_eq!(msg, "mic unplugged"),
        other => panic!("expected RouteError::Device, got {other:?}"),
    }

    let events = parse_events(&buf);
    assert_eq!(events.len(), 1, "exactly one worker event emitted");
    let ev = &events[0];
    assert_eq!(
        ev["event"].as_str(),
        Some("status"),
        "DeviceError emits a status line (which the UI dispatcher handles)",
    );
    assert_eq!(
        ev["state"].as_str(),
        Some("capture_lost"),
        "DeviceError sets `state=capture_lost` — the canonical UI state for an unrecoverable capture failure",
    );
    assert_eq!(
        ev["message"].as_str(),
        Some("mic unplugged"),
        "device-error message must round-trip through the payload",
    );
    assert_eq!(
        ev["backend"].as_str(),
        Some("rust-stdin"),
        "DeviceError tags the rust-stdin backend so the supervisor knows which restart path to take",
    );
}

/// SpeechStart / SpeechEnd round-trip through `on_event` as
/// `SpeechMarker::Start` / `SpeechMarker::End` **while a recording is
/// in flight**. The session state isn't mutated by either branch —
/// the route is purely translating.
#[test]
fn speech_start_and_speech_end_pass_through_while_recording() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let _cap_env = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    route.start_recording(&mut buf).expect("start_recording");

    let pre_state = route.session().state();
    let start = route
        .on_event(PipelineEvent::SpeechStart, &mut buf)
        .expect("SpeechStart");
    assert_eq!(start, Some(SpeechMarker::Start));
    assert_eq!(
        route.session().state(),
        pre_state,
        "SpeechStart must not mutate session state",
    );

    let end = route
        .on_event(PipelineEvent::SpeechEnd, &mut buf)
        .expect("SpeechEnd");
    assert_eq!(end, Some(SpeechMarker::End));
    assert_eq!(
        route.session().state(),
        pre_state,
        "SpeechEnd must not mutate session state",
    );
}

/// SpeechStart / SpeechEnd heard while the session is idle return
/// `None` so background speech between PTT presses doesn't trip stale
/// UI transitions in the live-preview / utterance-card consumers.
/// Codex P2 #415 audio_route.rs:290.
#[test]
fn speech_markers_dropped_while_session_idle() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    assert_eq!(route.session().state(), SessionState::Idle);

    let start = route
        .on_event(PipelineEvent::SpeechStart, &mut buf)
        .expect("SpeechStart");
    assert_eq!(start, None, "idle SpeechStart must not surface a marker");
    let end = route
        .on_event(PipelineEvent::SpeechEnd, &mut buf)
        .expect("SpeechEnd");
    assert_eq!(end, None, "idle SpeechEnd must not surface a marker");
    assert!(
        buf.is_empty(),
        "no worker events emitted for idle markers; got: {buf:?}",
    );
}

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

/// Codex P2 #415 audio_route.rs:250 — `start_recording` re-reads
/// `VOICEPI_MAX_RECORD_S` so a Settings save between PTT presses
/// takes effect on the next recording without rebuilding the route.
/// Construct with cap=None, then set the env var BEFORE start_recording
/// and assert the new cap fires on the next over-cap frame.
#[test]
fn start_recording_refreshes_max_record_seconds_from_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();

    // First recording: no cap → no capped event no matter how many
    // frames arrive.
    {
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0");
        route.start_recording(&mut buf).expect("start_recording");
        for _ in 0..5 {
            route
                .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
                .expect("frame under no-cap");
        }
        assert!(
            !route.cap_tripped(),
            "no cap → no cap-trip regardless of buffered duration",
        );
        // Settle the session before the next start_recording.
        route.stop_recording(&mut buf).expect("stop_recording");
    }

    // Second recording: env now caps at 0.03 s → next over-cap frame trips.
    {
        let _e = EnvVarGuard::set("VOICEPI_MAX_RECORD_S", "0.03");
        route.start_recording(&mut buf).expect("start_recording");
        // First frame fits (480 == cap).
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
            .expect("first frame");
        assert!(!route.cap_tripped());
        // Second trips.
        route
            .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
            .expect("trip frame");
        assert!(
            route.cap_tripped(),
            "env-refreshed cap must fire on next over-cap frame",
        );
    }

    // SR is used implicitly above (480 / SR = 0.03); reference it
    // here so a future change to the rate forces this test to fail
    // loudly rather than silently drift.
    assert_eq!(SR, 16_000, "max-record cap math assumes 16 kHz");
}
