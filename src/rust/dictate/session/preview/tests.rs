//! (`engine` / `emission` / `backend`) it exercises.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::backend::{PreviewBackend, PreviewError};
use super::emission::{build_preview_status, truncate_chars, PreviewEmission, PreviewSink};
use super::engine::{
    run_tick, PreviewEngine, PreviewEngineConfig, PreviewState, MIN_NEW_AUDIO_S,
    PREVIEW_MAX_AUDIO_S, PREVIEW_TEXT_CHARS,
};
use crate::dictate::events::WorkerStatus;

// ── helpers ──────────────────────────────────────────────────────────────

fn config_ms(interval_ms: u64, sample_rate: u32) -> PreviewEngineConfig {
    PreviewEngineConfig {
        interval: Duration::from_millis(interval_ms),
        sample_rate,
        min_new_audio_s: MIN_NEW_AUDIO_S,
        max_audio_s: PREVIEW_MAX_AUDIO_S,
        text_chars: PREVIEW_TEXT_CHARS,
    }
}

/// One second of silent 16 kHz mono PCM (matches
/// `MIN_NEW_AUDIO_S = 1.5` when pushed in two pieces).
fn one_second_pcm() -> Vec<f32> {
    vec![0.0f32; 16_000]
}

/// Capturing sink used by the thread-driven tests.
fn capturing_sink() -> (PreviewSink, Arc<Mutex<Vec<PreviewEmission>>>) {
    let captured: Arc<Mutex<Vec<PreviewEmission>>> = Arc::new(Mutex::new(Vec::new()));
    let handle = Arc::clone(&captured);
    let sink: PreviewSink = Arc::new(move |em| {
        handle.lock().unwrap().push(em);
    });
    (sink, captured)
}

/// Wait up to `total` for `pred` to hold, polling every 5 ms. Avoids
/// arbitrary sleeps in tests that watch the worker thread emit.
fn wait_until<F: FnMut() -> bool>(total: Duration, mut pred: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < total {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    pred()
}

struct StubBackend {
    text: String,
    calls: AtomicUsize,
}

impl StubBackend {
    fn new(text: &str) -> Arc<Self> {
        Arc::new(Self {
            text: text.to_owned(),
            calls: AtomicUsize::new(0),
        })
    }
}

impl PreviewBackend for StubBackend {
    fn transcribe_partial(&self, _pcm: &[f32], _sr: u32) -> Result<String, PreviewError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.text.clone())
    }
}

struct FailingBackend {
    calls: AtomicUsize,
}

impl FailingBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
}

impl PreviewBackend for FailingBackend {
    fn transcribe_partial(&self, _pcm: &[f32], _sr: u32) -> Result<String, PreviewError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(PreviewError::Backend("boom".to_owned()))
    }
}

/// Backend that blocks its `transcribe_partial` for `delay` before
/// returning `text`. Used by the stop-race regression test to simulate a
/// 200 ms model call so a mid-call `notify_stop` can be observed.
struct SlowBackend {
    text: String,
    delay: Duration,
    calls: AtomicUsize,
}

impl SlowBackend {
    fn new(text: &str, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            text: text.to_owned(),
            delay,
            calls: AtomicUsize::new(0),
        })
    }
}

impl PreviewBackend for SlowBackend {
    fn transcribe_partial(&self, _pcm: &[f32], _sr: u32) -> Result<String, PreviewError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        thread::sleep(self.delay);
        Ok(self.text.clone())
    }
}

// ── pure-state tests: fresh-audio gate + sliding window ─────────────────

/// audio (1.5) is the tick threshold, so 1 s of frames triggers 0
/// gate-skips (delta 1 s < 1.5 s), another 1 s (total 4 s) fires again.
#[test]
fn fresh_audio_gate_matches_python_min_new_audio_s() {
    let mut state = PreviewState::new(config_ms(1000, 16_000));
    state.on_start();

    // 1 s of frames: below the 1.5 s fresh-audio gate.
    state.on_frame(&one_second_pcm());
    assert!(
        state.take_tick().is_none(),
        "1 s < MIN_NEW_AUDIO_S (1.5 s) must skip"
    );

    // Total 2 s: delta = 2 s >= 1.5 s -> tick fires.
    state.on_frame(&one_second_pcm());
    let (pcm, total) = state.take_tick().expect("2 s clears the gate");
    assert_eq!(total, 2 * 16_000);
    assert_eq!(pcm.len(), 2 * 16_000, "below window cap -> full buffer");

    state.on_frame(&one_second_pcm());
    assert!(state.take_tick().is_none(), "delta 1 s < 1.5 s must skip");

    // Total 4 s: delta = 2 s -> tick fires.
    state.on_frame(&one_second_pcm());
    let (_pcm, total) = state.take_tick().expect("delta 2 s clears the gate");
    assert_eq!(total, 4 * 16_000);
}

/// After stop the accumulator drops and any subsequent take_tick is
/// suppressed -- final-pass path takes over. Mirrors Python's
/// `if not self._owner.recording: break`.
#[test]
fn take_tick_after_stop_returns_none() {
    let mut state = PreviewState::new(config_ms(1000, 16_000));
    state.on_start();
    state.on_frame(&vec![0.0f32; 4 * 16_000]);
    assert!(state.take_tick().is_some());

    state.on_stop();
    state.on_frame(&vec![0.0f32; 4 * 16_000]); // dropped, not recording
    assert!(
        state.take_tick().is_none(),
        "no tick after stop until next start"
    );
}

/// are trimmed to the recent tail. `total_samples` stays uncapped so
/// `recording_s` keeps tracking real elapsed audio.
#[test]
fn take_tick_windows_to_max_audio_seconds() {
    let sr = 16_000u32;
    let mut config = config_ms(500, sr);
    config.max_audio_s = 2.0; // trim to last 2 s for a short test
    let mut state = PreviewState::new(config);
    state.on_start();

    // 5 s of audio.
    state.on_frame(&vec![0.0f32; 5 * sr as usize]);
    let (pcm, total) = state.take_tick().expect("well above the gate");
    assert_eq!(total, 5 * sr as usize, "total is uncapped");
    assert_eq!(
        pcm.len(),
        2 * sr as usize,
        "windowed to the last max_audio_s seconds"
    );
}

// ── run_tick error/empty behaviour ──────────────────────────────────────

#[test]
fn run_tick_error_logs_once_and_does_not_emit() {
    let (sink, captured) = capturing_sink();
    let backend: Arc<dyn PreviewBackend> = FailingBackend::new() as Arc<dyn PreviewBackend>;
    let config = config_ms(500, 16_000);
    let stop_flag = AtomicBool::new(false);

    let mut state = PreviewState::new(config.clone());
    state.on_start();
    // Push enough audio to clear the fresh-audio gate.
    state.on_frame(&vec![0.0f32; 2 * 16_000]);
    let mut error_logged = false;
    run_tick(
        &mut state,
        &backend,
        &config,
        &sink,
        &mut error_logged,
        &stop_flag,
    );

    assert!(
        captured.lock().unwrap().is_empty(),
        "no emission on failure"
    );
    assert!(
        error_logged,
        "error flag set so subsequent failures stay quiet"
    );

    // Another tick with more audio -- gate cleared, backend still fails,
    // still no emission (and no panic).
    state.on_frame(&vec![0.0f32; 2 * 16_000]);
    run_tick(
        &mut state,
        &backend,
        &config,
        &sink,
        &mut error_logged,
        &stop_flag,
    );
    assert!(captured.lock().unwrap().is_empty());
}

#[test]
fn run_tick_empty_backend_output_does_not_emit() {
    let (sink, captured) = capturing_sink();
    let backend: Arc<dyn PreviewBackend> = StubBackend::new("   ");
    let config = config_ms(500, 16_000);
    let stop_flag = AtomicBool::new(false);

    let mut state = PreviewState::new(config.clone());
    state.on_start();
    state.on_frame(&vec![0.0f32; 2 * 16_000]);
    let mut error_logged = false;
    run_tick(
        &mut state,
        &backend,
        &config,
        &sink,
        &mut error_logged,
        &stop_flag,
    );
    assert!(
        captured.lock().unwrap().is_empty(),
        "whitespace-only text is skipped"
    );
}

/// the worker has NOT yet drained a `Stop` message from its channel),
/// setting the shared stop flag between `take_tick` and the sink call
/// must suppress the emission. This is the pre-worker unit-test for the
/// same invariant the thread-driven `engine_suppresses_emission_when_stop_races_transcribe`
/// test exercises end-to-end.
#[test]
fn run_tick_stop_flag_suppresses_emission_even_while_recording() {
    let (sink, captured) = capturing_sink();
    let backend: Arc<dyn PreviewBackend> = StubBackend::new("hej");
    let config = config_ms(500, 16_000);
    let stop_flag = AtomicBool::new(true); // simulate a stop signalled mid-transcribe

    let mut state = PreviewState::new(config.clone());
    state.on_start();
    state.on_frame(&vec![0.0f32; 2 * 16_000]);
    assert!(state.is_recording(), "stop flag alone must be enough");
    let mut error_logged = false;
    run_tick(
        &mut state,
        &backend,
        &config,
        &sink,
        &mut error_logged,
        &stop_flag,
    );
    assert!(
        captured.lock().unwrap().is_empty(),
        "stop-race flag must gate emission before it hits the sink"
    );
}

// ── payload wire shape ──────────────────────────────────────────────────

/// Pins the emitted status-event shape byte-equivalent to Python's
/// `recording_s` (float, 2 dp). If ANY of those drift the UI's
#[test]
fn preview_event_payload_shape_matches_python() {
    let emission = PreviewEmission {
        text: "hej verden".to_owned(),
        recording_s: 1.2345,
    };
    let event = build_preview_status(&emission);
    assert_eq!(event.state, WorkerStatus::Preview);
    assert_eq!(event.state.as_wire_str(), "preview");
    assert_eq!(
        event.extras.get("text_preview").and_then(|v| v.as_str()),
        Some("hej verden")
    );
    // Rounded to 2 dp, matching Python's round(samples/rate, 2).
    assert_eq!(
        event.extras.get("recording_s").and_then(|v| v.as_f64()),
        Some(1.23)
    );
}

/// Text longer than `text_chars` is truncated char-safely (not byte-
/// unsafely, so multi-byte UTF-8 does not panic).
#[test]
fn preview_text_is_truncated_char_safely() {
    assert_eq!(truncate_chars("hello", 3), "hel");
    assert_eq!(truncate_chars("æøå", 2), "æø");
    assert_eq!(truncate_chars("short", 10), "short");
    assert_eq!(truncate_chars("anything", 0), "");
}

// ── engine lifecycle (thread-driven) ────────────────────────────────────

#[test]
fn engine_emits_after_interval_when_gate_clears() {
    let (sink, captured) = capturing_sink();
    let backend: Arc<dyn PreviewBackend> = StubBackend::new("hej");
    let mut config = config_ms(30, 16_000);
    config.min_new_audio_s = 0.0; // let the first tick fire
    let engine = PreviewEngine::spawn(Arc::clone(&backend), config, sink);
    engine.notify_start();
    engine.push_frame(&one_second_pcm());

    let got = wait_until(Duration::from_secs(2), || {
        !captured.lock().unwrap().is_empty()
    });
    assert!(got, "engine must emit at least one preview");
    let first = captured.lock().unwrap()[0].clone();
    assert_eq!(first.text, "hej");
    assert!(first.recording_s > 0.0);

    drop(engine); // joins the worker
}

#[test]
fn engine_stops_emitting_after_notify_stop() {
    let (sink, captured) = capturing_sink();
    let backend = StubBackend::new("word");
    let backend_dyn: Arc<dyn PreviewBackend> = Arc::clone(&backend) as Arc<dyn PreviewBackend>;
    let mut config = config_ms(20, 16_000);
    config.min_new_audio_s = 0.0;
    let engine = PreviewEngine::spawn(backend_dyn, config, sink);

    engine.notify_start();
    engine.push_frame(&one_second_pcm());
    wait_until(Duration::from_millis(500), || {
        !captured.lock().unwrap().is_empty()
    });
    assert!(!captured.lock().unwrap().is_empty(), "warm-up emission");

    engine.notify_stop();
    let baseline = captured.lock().unwrap().len();
    // Push more audio AFTER stop: worker must drop it, no further
    // emissions -- final-pass path is the only source of truth once
    // stop was signalled.
    engine.push_frame(&one_second_pcm());
    engine.push_frame(&one_second_pcm());
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        captured.lock().unwrap().len(),
        baseline,
        "no previews after notify_stop"
    );

    drop(engine);
}

#[test]
fn engine_swallows_backend_errors() {
    let (sink, captured) = capturing_sink();
    let backend = FailingBackend::new();
    let backend_dyn: Arc<dyn PreviewBackend> = Arc::clone(&backend) as Arc<dyn PreviewBackend>;
    let mut config = config_ms(20, 16_000);
    config.min_new_audio_s = 0.0;
    let engine = PreviewEngine::spawn(backend_dyn, config, sink);

    engine.notify_start();
    engine.push_frame(&one_second_pcm());
    // Wait long enough that at least one tick was attempted.
    thread::sleep(Duration::from_millis(200));
    engine.notify_stop();

    assert!(
        captured.lock().unwrap().is_empty(),
        "failing backend never emits"
    );
    assert!(
        backend.calls.load(Ordering::SeqCst) >= 1,
        "backend was invoked at least once (worker did not crash on the first failure)"
    );

    drop(engine);
}

#[test]
fn from_seconds_disabled_when_zero_or_negative() {
    assert!(PreviewEngineConfig::from_seconds(0.0, 16_000).is_none());
    assert!(PreviewEngineConfig::from_seconds(-1.0, 16_000).is_none());
    assert!(PreviewEngineConfig::from_seconds(3.0, 16_000).is_some());
}

///
/// `transcribe_partial` call for 200 ms; fresh-audio gate zeroed so the
/// first tick fires immediately.
///
/// Timeline:
///   t=0    engine spawn + notify_start + push_frame(1 s of PCM).
///   t≈30ms worker fires the first tick -> transcribe_partial enters its
///          200 ms sleep.
///   t≈130ms test thread calls notify_stop -> shared stop flag stores
///          but the worker is still parked inside transcribe_partial.
///   t≈230ms transcribe_partial returns "delayed"; run_tick's Acquire
///          load observes the stop flag and drops the emission on the
///          floor.
///
/// Assertion: after a 500 ms grace window the capturing sink is still
/// empty. Without the atomic guard the sink would have received one
/// stale "delayed" emission AFTER the session had already emitted its
/// scenario the   described).
#[test]
fn engine_suppresses_emission_when_stop_races_transcribe() {
    let (sink, captured) = capturing_sink();
    let backend: Arc<dyn PreviewBackend> =
        SlowBackend::new("delayed", Duration::from_millis(200)) as Arc<dyn PreviewBackend>;
    let mut config = config_ms(30, 16_000);
    config.min_new_audio_s = 0.0; // let the first tick fire on push_frame
    let engine = PreviewEngine::spawn(backend, config, sink);

    engine.notify_start();
    engine.push_frame(&one_second_pcm());

    // Let the worker enter transcribe_partial's 200 ms sleep, then stop
    // mid-flight (the 's 100 ms window).
    thread::sleep(Duration::from_millis(100));
    engine.notify_stop();

    // Wait past the backend's delay AND a generous grace window for any
    // late emission to arrive.
    thread::sleep(Duration::from_millis(500));

    assert!(
        captured.lock().unwrap().is_empty(),
        "stop-during-preview must suppress the pending emission (#608 \
         preview.rs:245); saw {:?}",
        *captured.lock().unwrap()
    );

    drop(engine);
}
