//! Test doubles for the push-to-talk capture lifecycle: a fake opener that
//! tracks open streams, a recording frame sink, and runtime-channel helpers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

use super::capture_forwarder::FrameSink;
use super::capture_lifecycle::{CaptureLifecycle, CaptureOpener};
use super::capture_status::CaptureReporter;
use crate::audio::bounded_queue::LatestSender;
use crate::audio::raw::pipeline_event_channel;
use crate::audio::{PipelineEvent, PipelineReceiver};
use crate::runtime::RuntimeEvent;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FakeFailure {
    Error,
    Timeout,
}

#[derive(Default)]
struct FakeState {
    opened: Vec<String>,
    probed: Vec<String>,
    missing: Vec<String>,
    failures: HashMap<String, FakeFailure>,
    frames_on_open: Vec<Vec<f32>>,
    sender: Option<LatestSender<PipelineEvent>>,
}

type OpenHook = Box<dyn Fn() + Send + Sync>;

/// Fake capture opener. Each successful open creates a stream whose drop
/// disconnects the receiver, mirroring `RawCapturePipeline`.
#[derive(Clone, Default)]
pub(super) struct FakeOpener {
    state: Arc<Mutex<FakeState>>,
    open_streams: Arc<AtomicUsize>,
    on_open: Arc<OnceLock<OpenHook>>,
}

pub(super) struct FakeStream {
    state: Arc<Mutex<FakeState>>,
    open_streams: Arc<AtomicUsize>,
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        lock(&self.state).sender = None;
        self.open_streams.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FakeOpener {
    pub(super) fn fail(&self, selector: &str, failure: FakeFailure) {
        lock(&self.state)
            .failures
            .insert(selector.to_owned(), failure);
    }

    pub(super) fn clear_failure(&self, selector: &str) {
        lock(&self.state).failures.remove(selector);
    }

    pub(super) fn missing_at_startup(&self, selector: &str) {
        lock(&self.state).missing.push(selector.to_owned());
    }

    /// Frames the device delivers immediately after every open.
    pub(super) fn deliver_on_open(&self, frame: Vec<f32>) {
        lock(&self.state).frames_on_open.push(frame);
    }

    /// Push an event on the currently open stream. False when closed.
    pub(super) fn feed(&self, event: PipelineEvent) -> bool {
        lock(&self.state)
            .sender
            .as_ref()
            .is_some_and(|sender| sender.try_send_latest(event).is_ok())
    }

    pub(super) fn on_open(&self, hook: impl Fn() + Send + Sync + 'static) {
        assert!(self.on_open.set(Box::new(hook)).is_ok(), "hook already set");
    }

    pub(super) fn opened(&self) -> Vec<String> {
        lock(&self.state).opened.clone()
    }

    pub(super) fn probed(&self) -> Vec<String> {
        lock(&self.state).probed.clone()
    }

    pub(super) fn open_streams(&self) -> usize {
        self.open_streams.load(Ordering::SeqCst)
    }
}

impl CaptureOpener for FakeOpener {
    type Stream = FakeStream;

    fn probe(&self, selector: &str) -> Result<(), anyhow::Error> {
        let mut state = lock(&self.state);
        state.probed.push(selector.to_owned());
        if state.missing.iter().any(|missing| missing == selector) {
            return Err(anyhow::anyhow!("input device not found: {selector}"));
        }
        Ok(())
    }

    fn open(&self, selector: &str) -> Result<(FakeStream, PipelineReceiver), anyhow::Error> {
        if let Some(hook) = self.on_open.get() {
            hook();
        }
        let mut state = lock(&self.state);
        state.opened.push(selector.to_owned());
        match state.failures.get(selector) {
            Some(FakeFailure::Error) => return Err(anyhow::anyhow!("fake open failed")),
            Some(FakeFailure::Timeout) => return Err(anyhow::anyhow!("fake open timed out")),
            None => {}
        }
        let (tx, rx) = pipeline_event_channel(1024);
        for frame in state.frames_on_open.clone() {
            let _ = tx.try_send_latest(PipelineEvent::Frame(frame));
        }
        state.sender = Some(tx);
        self.open_streams.fetch_add(1, Ordering::SeqCst);
        Ok((
            FakeStream {
                state: Arc::clone(&self.state),
                open_streams: Arc::clone(&self.open_streams),
            },
            rx,
        ))
    }

    fn is_timeout(&self, error: &anyhow::Error) -> bool {
        error.to_string().contains("timed out")
    }
}

/// Frame sink that records frames and can pretend the session is busy.
#[derive(Default)]
pub(super) struct RecordingFrames {
    frames: Mutex<Vec<Vec<f32>>>,
    busy: AtomicBool,
    attempts: AtomicUsize,
}

impl RecordingFrames {
    pub(super) fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
    }

    pub(super) fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    pub(super) fn frames(&self) -> Vec<Vec<f32>> {
        lock(&self.frames).clone()
    }
}

impl FrameSink for RecordingFrames {
    fn try_push(&self, frame: &[f32]) -> bool {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.busy.load(Ordering::SeqCst) {
            return false;
        }
        lock(&self.frames).push(frame.to_vec());
        true
    }
}

pub(super) struct ReporterRig {
    pub(super) rx: mpsc::Receiver<RuntimeEvent>,
    pub(super) effective_device: Arc<RwLock<String>>,
}

impl ReporterRig {
    /// Every event published so far.
    pub(super) fn drain(&self) -> Vec<RuntimeEvent> {
        self.rx.try_iter().collect()
    }

    pub(super) fn effective_device(&self) -> String {
        self.effective_device.read().unwrap().clone()
    }
}

/// Build a lifecycle over `opener` + `frames` configured for `device`.
pub(super) fn lifecycle_with<F: FrameSink>(
    opener: &FakeOpener,
    frames: Arc<F>,
    device: &str,
) -> Result<(CaptureLifecycle<FakeOpener, F>, ReporterRig), anyhow::Error> {
    let (tx, rx) = mpsc::channel();
    let effective_device = Arc::new(RwLock::new(device.to_owned()));
    let reporter = CaptureReporter::new(tx, None, Arc::clone(&effective_device));
    let lifecycle = CaptureLifecycle::new(opener.clone(), frames, device, reporter)?;
    Ok((
        lifecycle,
        ReporterRig {
            rx,
            effective_device,
        },
    ))
}

/// `(state, payload)` for every worker status event.
pub(super) fn statuses(events: &[RuntimeEvent]) -> Vec<(String, serde_json::Value)> {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Worker(worker) => Some((
                worker.state.clone().unwrap_or_default(),
                worker.payload.clone(),
            )),
            _ => None,
        })
        .collect()
}

pub(super) fn stderr_lines(events: &[RuntimeEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Stderr(line) => Some(line.clone()),
            _ => None,
        })
        .collect()
}

/// Poll `condition` for up to five seconds.
pub(super) fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
