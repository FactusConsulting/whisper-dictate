//! Engine tests for the stop flag reaching an in-flight
//! `transcribe_partial` and for the bounded frame backlog.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    seconds_to_samples, PreviewEngine, PreviewEngineConfig, FRAME_BACKLOG_MAX_S,
    PREVIEW_MAX_AUDIO_S, PREVIEW_TEXT_CHARS,
};
use crate::dictate::session::preview::backend::{PreviewBackend, PreviewError};
use crate::dictate::session::preview::emission::PreviewSink;

const SAMPLE_RATE: u32 = 16_000;

/// How long the fake backend waits for the stop flag before giving up. Far
/// above the promptness bound the tests assert, so a flag that never reaches
/// the backend shows up as a slow stop instead of a hung test.
const BACKEND_GIVE_UP: Duration = Duration::from_secs(5);

/// Bound on `notify_stop` + `drop` with an in-flight tick.
const PROMPT: Duration = Duration::from_secs(1);

/// Stands in for a long whisper.cpp inference: blocks inside
/// `transcribe_partial` until it observes the cancel flag.
#[derive(Default)]
struct BlockUntilCancelled {
    entered: AtomicBool,
    saw_cancel: AtomicBool,
}

impl PreviewBackend for BlockUntilCancelled {
    fn transcribe_partial(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
        cancel: &AtomicBool,
    ) -> Result<String, PreviewError> {
        self.entered.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + BACKEND_GIVE_UP;
        while !cancel.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err(PreviewError::Backend("cancel never observed".to_owned()));
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.saw_cancel.store(true, Ordering::SeqCst);
        Ok("stale".to_owned())
    }
}

fn counting_sink() -> (PreviewSink, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let handle = Arc::clone(&count);
    let sink: PreviewSink = Arc::new(move |_| {
        handle.fetch_add(1, Ordering::SeqCst);
    });
    (sink, count)
}

fn fast_config() -> PreviewEngineConfig {
    PreviewEngineConfig {
        interval: Duration::from_millis(10),
        sample_rate: SAMPLE_RATE,
        min_new_audio_s: 0.0,
        max_audio_s: PREVIEW_MAX_AUDIO_S,
        text_chars: PREVIEW_TEXT_CHARS,
    }
}

/// Start a recording and return once the worker is blocked inside
/// `transcribe_partial`.
fn engine_blocked_in_tick() -> (PreviewEngine, Arc<BlockUntilCancelled>, Arc<AtomicUsize>) {
    let backend = Arc::new(BlockUntilCancelled::default());
    let (sink, emitted) = counting_sink();
    let engine = PreviewEngine::spawn(
        Arc::clone(&backend) as Arc<dyn PreviewBackend>,
        fast_config(),
        sink,
    );
    engine.notify_start();
    engine.push_frame(&vec![0.0; SAMPLE_RATE as usize]);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !backend.entered.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "worker never entered a tick");
        thread::sleep(Duration::from_millis(1));
    }
    (engine, backend, emitted)
}

#[test]
fn notify_stop_aborts_the_in_flight_partial_and_drop_returns_promptly() {
    let (engine, backend, emitted) = engine_blocked_in_tick();

    let started = Instant::now();
    engine.notify_stop();
    drop(engine);
    let elapsed = started.elapsed();

    assert!(elapsed < PROMPT, "stop + drop took {elapsed:?}");
    assert!(backend.saw_cancel.load(Ordering::SeqCst));
    assert_eq!(emitted.load(Ordering::SeqCst), 0, "aborted tick emitted");
}

#[test]
fn drop_without_stop_aborts_the_in_flight_partial() {
    let (engine, backend, emitted) = engine_blocked_in_tick();

    let started = Instant::now();
    drop(engine);
    let elapsed = started.elapsed();

    assert!(elapsed < PROMPT, "drop took {elapsed:?}");
    assert!(backend.saw_cancel.load(Ordering::SeqCst));
    assert_eq!(emitted.load(Ordering::SeqCst), 0, "aborted tick emitted");
}

#[test]
fn frames_pushed_while_the_worker_is_busy_are_capped_and_counted() {
    let (engine, _backend, _emitted) = engine_blocked_in_tick();
    let cap = seconds_to_samples(FRAME_BACKLOG_MAX_S, SAMPLE_RATE);
    let frame = vec![0.0_f32; SAMPLE_RATE as usize];
    // Normally 0; the setup frame is still queued if the first tick fired
    // before it arrived.
    let already = engine.backlog.queued_samples.load(Ordering::SeqCst);
    let accepted = (cap - already) / frame.len();

    for _ in 0..accepted + 25 {
        engine.push_frame(&frame);
    }

    assert_eq!(
        engine.backlog.queued_samples.load(Ordering::SeqCst),
        already + accepted * frame.len(),
        "backlog must stop growing at the cap"
    );
    assert_eq!(engine.backlog.dropped_frames.load(Ordering::SeqCst), 25);
    engine.notify_stop();
}

#[test]
fn an_oversized_frame_is_accepted_into_an_empty_backlog() {
    let (sink, _emitted) = counting_sink();
    let backend = Arc::new(BlockUntilCancelled::default());
    let engine = PreviewEngine::spawn(backend, fast_config(), sink);
    let cap = seconds_to_samples(FRAME_BACKLOG_MAX_S, SAMPLE_RATE);

    engine.push_frame(&vec![0.0; cap + 1]);

    assert_eq!(engine.backlog.dropped_frames.load(Ordering::SeqCst), 0);
}
