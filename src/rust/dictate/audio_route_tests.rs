//! Unit tests for [`super::audio_route::AudioRoute`].
//!
//! Covers the four behaviour gates the route mirrors from
//! `src/python/whisper_dictate/vp_capture_rust_stdin.py`:
//!
//! 1. **First-frame race** — the first `PipelineEvent::Frame` arriving
//!    immediately after `start_recording()` MUST land in the session
//!    buffer (no Opening/Recording handshake race).
//! 2. **Idle-frame drop** — frames that arrive while the session is
//!    idle (no `start_recording` issued) are dropped silently.
//! 3. **Max-record cap** — once the buffered duration exceeds
//!    `VOICEPI_MAX_RECORD_S`, the route stops accepting frames AND
//!    triggers an automatic `stop_and_transcribe`.
//! 4. **DeviceError** — a `PipelineEvent::DeviceError` emits an
//!    `event=error` worker line and returns `RouteError::Device`.
//!
//! Plus a `SpeechStart` / `SpeechEnd` / `Cancelled` pass-through
//! coverage trio so the supervisor-facing return shape can't drift.
//!
//! The tests construct the route via `AudioRoute::for_test`, which
//! skips the real cpal pipeline — every `PipelineEvent` flows through
//! `on_event` directly, the same path the supervisor (PR 4) will use.
//! No cpal / Silero / Whisper dependency in the test binary.

use std::cell::RefCell;

use serde_json::Value;

use crate::audio::PipelineEvent;
use crate::dictate::audio_route::{AudioRoute, RouteConfig, RouteError, SpeechMarker};
use crate::dictate::session::{
    DictateSession, InjectBackend, InjectError, SessionConfig, SessionState, TranscribeBackend,
    TranscribeError, TranscribeResult, UtteranceOutcome, SR,
};
use crate::test_env_lock::ENV_LOCK;

// ── test backends ────────────────────────────────────────────────────────────

/// Minimal transcribe backend: returns a fixed non-empty text so the
/// session's `stop_and_transcribe` resolves to `Injected` and we can
/// observe the buffered sample count handed to the model.
struct TestTranscribe {
    seen_pcm_len: RefCell<Vec<usize>>,
}

impl TestTranscribe {
    fn new() -> Self {
        Self {
            seen_pcm_len: RefCell::new(Vec::new()),
        }
    }
}

impl TranscribeBackend for TestTranscribe {
    fn transcribe(
        &self,
        pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        self.seen_pcm_len.borrow_mut().push(pcm.len());
        Ok(TranscribeResult {
            text: "hello".into(),
            ..TranscribeResult::default()
        })
    }
}

/// Minimal inject backend: stash every injected string so the test can
/// assert end-to-end the auto-stop-on-cap actually produced an
/// utterance.
struct TestInject {
    injected: RefCell<Vec<String>>,
}

impl TestInject {
    fn new() -> Self {
        Self {
            injected: RefCell::new(Vec::new()),
        }
    }
}

impl InjectBackend for TestInject {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        self.injected.borrow_mut().push(text.into());
        Ok(())
    }
}

// ── env helper ───────────────────────────────────────────────────────────────

/// Set `VOICEPI_WORKER_EVENTS=1` for the duration of a test under the
/// crate-wide [`ENV_LOCK`]. The `events::emit_error` call inside
/// `on_event(DeviceError)` is gated on this variable, and the session's
/// own event lines (recording / transcribing / ready) likewise. The
/// guard restores the original value on drop.
///
/// Returns `(MutexGuard, EnvVarGuard)` — both MUST be bound for the
/// scope of the test (`let _g = ...; let _e = ...;`).
struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn route_with_cap(cap_seconds: Option<f64>) -> AudioRoute<TestTranscribe, TestInject> {
    let session = DictateSession::new(
        TestTranscribe::new(),
        TestInject::new(),
        SessionConfig {
            // Drop the min-record floor to zero so even tiny cap-trip
            // buffers transcribe (otherwise the `Skipped` branch fires
            // and the auto-stop test can't assert on `Injected`).
            min_record_seconds: 0.0,
            ..SessionConfig::default()
        },
    );
    AudioRoute::for_test(
        session,
        RouteConfig {
            max_record_seconds: cap_seconds,
        },
    )
}

fn parse_events(bytes: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(bytes).expect("event stream is UTF-8");
    let mut events = Vec::new();
    for line in text.lines() {
        if let Some(payload) = line.strip_prefix("[worker-event] ") {
            events.push(
                serde_json::from_str(payload).expect("worker-event payload must be valid JSON"),
            );
        }
    }
    events
}

// ── tests ────────────────────────────────────────────────────────────────────

/// Gate 1 — the first `Frame` after `start_recording()` lands in the
/// buffer. Guards against a regression where the route checks state
/// before the session has finished transitioning to Recording.
#[test]
fn first_frame_after_start_lands_in_session_buffer() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
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
/// stops accepting frames AND triggers an automatic
/// `stop_and_transcribe`. After the trip, the session is back in Idle
/// and a follow-up frame is dropped.
///
/// Cap = 0.36 s = 5760 samples at 16 kHz (12 × 480-sample frames). We
/// pick a cap above the 0.3 s misfire floor in
/// [`crate::dictate::skip::MIN_RECORD_FLOOR_S`] so the auto-stop reaches
/// the transcriber rather than the `Skipped { reason: "too_short" }`
/// branch — otherwise the cap-trip path would never exercise the
/// transcribe-backend assertion below.
#[test]
fn max_record_cap_auto_stops_recording_and_drops_subsequent_frames() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(Some(0.36)); // 5760 samples
    let mut buf = Vec::new();
    route.start_recording(&mut buf).expect("start_recording");

    // First 12 frames fit (12 × 480 = 5760 == cap).
    for _ in 0..12 {
        let frame: Vec<f32> = vec![0.5; 480];
        route
            .on_event(PipelineEvent::Frame(frame), &mut buf)
            .expect("on_event(Frame) within cap");
    }
    assert!(
        matches!(route.session().state(), SessionState::Recording { .. }),
        "session must still be Recording after under-cap frames",
    );
    assert_eq!(route.buffered_samples(), 5760);

    // 13th frame would push to 6240 samples = 0.39 s > 0.36 s cap.
    // Expect auto-stop + the trip-frame dropped.
    let trip_frame: Vec<f32> = vec![0.5; 480];
    route
        .on_event(PipelineEvent::Frame(trip_frame), &mut buf)
        .expect("on_event(Frame) cap-trip must not error");
    assert!(
        route.cap_tripped(),
        "cap_tripped must be set after the trip frame",
    );
    assert_eq!(
        route.session().state(),
        SessionState::Idle,
        "auto-stop must drive the session back to Idle",
    );

    // A subsequent frame is dropped (state=Idle gate catches it).
    let extra: Vec<f32> = vec![0.5; 480];
    route
        .on_event(PipelineEvent::Frame(extra), &mut buf)
        .expect("on_event(Frame) after auto-stop must not error");

    // The transcribe backend saw the under-cap buffer (5760 samples).
    let pcm_lens: Vec<usize> = route
        .session()
        // Reach into the backend via the test-only helper on the
        // session. The transcribe backend captured every PCM length
        // it was handed.
        .transcribe_backend()
        .seen_pcm_len
        .borrow()
        .clone();
    assert_eq!(
        pcm_lens,
        vec![5760],
        "auto-stop must transcribe the under-cap buffer exactly once",
    );

    // The session walked the full recording → transcribing → ready
    // state ladder; assert the recording/transcribing/ready trio is
    // present so the UI consumers (which key on the sequence) keep
    // working when the cap fires.
    let events = parse_events(&buf);
    let states: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("state").and_then(|s| s.as_str()))
        .collect();
    assert!(
        states.contains(&"recording")
            && states.contains(&"transcribing")
            && states.contains(&"ready"),
        "auto-stop must emit recording/transcribing/ready state ladder; got {states:?}",
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

/// Gate 4 — a `PipelineEvent::DeviceError` emits an `event=error`
/// worker line and returns `RouteError::Device(message)`.
#[test]
fn device_error_emits_error_event_and_returns_route_error() {
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
    assert_eq!(ev["event"].as_str(), Some("error"));
    assert_eq!(
        ev["message"].as_str(),
        Some("mic unplugged"),
        "device-error message must round-trip through the payload",
    );
    assert_eq!(
        ev["state"].as_str(),
        Some("error"),
        "DeviceError sets the canonical `state=error` field",
    );
    assert_eq!(
        ev["backend"].as_str(),
        Some("rust-stdin"),
        "DeviceError tags the rust-stdin backend so the supervisor knows which restart path to take",
    );
}

/// SpeechStart / SpeechEnd round-trip through `on_event` as
/// `SpeechMarker::Start` / `SpeechMarker::End`. The session state
/// isn't mutated by either branch — the route is purely translating.
#[test]
fn speech_start_and_speech_end_pass_through_as_markers() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
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

/// `PipelineEvent::Cancelled` runs through `DictateSession::cancel`
/// with the active epoch — the session settles back to Idle and the
/// route's buffered-sample counter resets so the next recording is
/// clean.
#[test]
fn cancelled_event_settles_session_to_idle_and_resets_buffer() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(None);
    let mut buf = Vec::new();
    route.start_recording(&mut buf).expect("start_recording");

    // Push a couple of frames so the route has non-zero buffered
    // samples to reset.
    for _ in 0..2 {
        let frame: Vec<f32> = vec![0.5; 480];
        route
            .on_event(PipelineEvent::Frame(frame), &mut buf)
            .expect("Frame");
    }
    assert_eq!(route.buffered_samples(), 960);

    let marker = route
        .on_event(PipelineEvent::Cancelled, &mut buf)
        .expect("Cancelled");
    assert_eq!(marker, None, "Cancelled returns no SpeechMarker");
    assert_eq!(
        route.session().state(),
        SessionState::Idle,
        "Cancelled must drive the session back to Idle",
    );
    assert_eq!(
        route.buffered_samples(),
        0,
        "Cancelled must reset the route's buffered-sample counter",
    );

    let events = parse_events(&buf);
    let states: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("state").and_then(|s| s.as_str()))
        .collect();
    assert!(
        states.contains(&"cancelled"),
        "Cancelled must surface a cancelled status event; got {states:?}",
    );
}

/// Bonus — a second `stop_recording` after a cap-trip auto-stop is a
/// no-op (returns `NotRecording`). Guards against a double-stop
/// regression where the supervisor releases PTT after the cap fired
/// and we accidentally re-transcribe an empty buffer.
#[test]
fn stop_recording_after_cap_trip_is_no_op() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvVarGuard::set("VOICEPI_WORKER_EVENTS", "1");
    let mut route = route_with_cap(Some(0.03)); // 480 samples = exactly one frame
    let mut buf = Vec::new();
    route.start_recording(&mut buf).expect("start_recording");

    // One frame fits (480 == cap). Second frame trips and auto-stops.
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("first frame");
    route
        .on_event(PipelineEvent::Frame(vec![0.5; 480]), &mut buf)
        .expect("trip frame");
    assert_eq!(route.session().state(), SessionState::Idle);

    // Supervisor releases PTT — this stop is a no-op.
    let outcome = route.stop_recording(&mut buf).expect("stop_recording");
    assert!(
        matches!(outcome, UtteranceOutcome::NotRecording),
        "second stop after cap-trip auto-stop must be a no-op; got {outcome:?}",
    );

    // SR is used implicitly above (480 / SR = 0.03); reference it
    // here so a future change to the rate forces this test to fail
    // loudly rather than silently drift.
    assert_eq!(SR, 16_000, "max-record cap math assumes 16 kHz");
}
