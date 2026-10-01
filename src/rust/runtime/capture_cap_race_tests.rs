//! A live producer overflows after the sink observes the recording cap, but
//! before it returns. This also exercises the pending-frame flush path.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::audio::bounded_queue::OverflowMetric;
use crate::audio::raw::pipeline_event_channel;
use crate::audio::PipelineReceiver;
use crate::runtime::capture_forwarder::{
    forward_frames_with_report_and_cap, FrameSink, PushOutcome,
};
use crate::runtime::capture_lifecycle::{CaptureLifecycle, CaptureOpener};
use crate::runtime::capture_status::CaptureReporter;
use crate::runtime::capture_test_support::{stderr_lines, wait_until, ReporterRig};
use crate::runtime::recording_capture::RecordingCapture;
use crate::runtime::RuntimeEvent;

const WAIT: Duration = Duration::from_secs(3);

struct LiveCapOpener {
    cap_rx: Mutex<Option<mpsc::Receiver<()>>>,
    done_tx: mpsc::Sender<()>,
    producer_done: Arc<AtomicBool>,
    metric: Arc<Mutex<Option<OverflowMetric>>>,
    initial_loss: bool,
}

struct LiveCapStream(Option<JoinHandle<()>>);

impl Drop for LiveCapStream {
    fn drop(&mut self) {
        self.0.take().unwrap().join().unwrap();
    }
}

impl CaptureOpener for LiveCapOpener {
    type Stream = LiveCapStream;

    fn probe(&self, _selector: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn open(&self, _selector: &str) -> anyhow::Result<(Self::Stream, PipelineReceiver)> {
        let (events, receiver) = pipeline_event_channel(2);
        for value in 0..(2 + usize::from(self.initial_loss)) {
            events
                .try_send_latest(crate::audio::PipelineEvent::Frame(vec![value as f32]))
                .unwrap();
        }
        *self.metric.lock().unwrap() = Some(events.overflow_metric());
        let cap_rx = self.cap_rx.lock().unwrap().take().unwrap();
        let done_tx = self.done_tx.clone();
        let producer_done = Arc::clone(&self.producer_done);
        let producer = thread::spawn(move || {
            cap_rx.recv_timeout(WAIT).unwrap();
            for _ in 0..64 {
                events
                    .try_send_latest(crate::audio::PipelineEvent::Frame(vec![0.5]))
                    .unwrap();
            }
            producer_done.store(true, Ordering::Release);
            done_tx.send(()).unwrap();
        });
        Ok((LiveCapStream(Some(producer)), receiver))
    }

    fn is_timeout(&self, _error: &anyhow::Error) -> bool {
        false
    }
}

struct PausingCapSink {
    ready_rx: Mutex<Option<mpsc::Receiver<()>>>,
    busy_once: AtomicBool,
    cap_tx: mpsc::Sender<()>,
    done_rx: Mutex<mpsc::Receiver<()>>,
}

impl FrameSink for PausingCapSink {
    fn try_push_with_cap(&self, _frame: &[f32], on_full: &mut dyn FnMut()) -> PushOutcome {
        // The lifecycle must announce the open before this fast fixture caps.
        if let Some(ready) = self.ready_rx.lock().unwrap().take() {
            ready.recv_timeout(WAIT).unwrap();
        }
        if self.busy_once.swap(false, Ordering::SeqCst) {
            return PushOutcome::Busy;
        }
        on_full();
        self.cap_tx.send(()).unwrap();
        // Widen the exact post-cap/pre-return window, without sleeps or audio
        // hardware. A snapshot after forwarding returns would include the loss.
        self.done_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
        PushOutcome::RecordingFull
    }
}

fn assert_cap_cutoff(busy_first: bool, initial_loss: bool) {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (cap_tx, cap_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let producer_done = Arc::new(AtomicBool::new(false));
    let metric = Arc::new(Mutex::new(None));
    let opener = LiveCapOpener {
        cap_rx: Mutex::new(Some(cap_rx)),
        done_tx,
        producer_done: Arc::clone(&producer_done),
        metric: Arc::clone(&metric),
        initial_loss,
    };
    let sink = Arc::new(PausingCapSink {
        ready_rx: Mutex::new(Some(ready_rx)),
        busy_once: AtomicBool::new(busy_first),
        cap_tx,
        done_rx: Mutex::new(done_rx),
    });
    let (tx, rx) = mpsc::channel();
    let effective_device = Arc::new(RwLock::new(String::new()));
    let reporter = CaptureReporter::new(tx, None, Arc::clone(&effective_device));
    let lifecycle = CaptureLifecycle::new(opener, sink, "", reporter);
    let rig = ReporterRig {
        rx,
        effective_device,
    };
    assert!(lifecycle.open_for_recording());
    ready_tx.send(()).unwrap();
    wait_until("live post-cap producer", || {
        producer_done.load(Ordering::Acquire)
    });
    lifecycle.close_for_recording();
    let initial_dropped = u64::from(initial_loss);
    assert!(metric.lock().unwrap().as_ref().unwrap().count() > initial_dropped);
    let events = rig.drain();
    let overflow: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Worker(worker) if worker.event == "audio_overflow" => Some(worker),
            _ => None,
        })
        .collect();
    assert_eq!(overflow.len(), usize::from(initial_loss));
    if let Some(worker) = overflow.first() {
        assert_eq!(worker.payload["capture_chunks_dropped"], 0);
        assert_eq!(worker.payload["pipeline_events_dropped"], initial_dropped);
        assert_eq!(worker.payload["pending_frames_dropped"], 0);
    }
    let lines = stderr_lines(&events);
    assert!(lines.iter().any(|line| line.contains("max_record_s")));
    assert_eq!(
        lines.iter().any(|line| line.contains("incomplete")),
        initial_loss
    );
}

#[test]
fn direct_cap_ignores_live_producer_loss_before_sink_returns() {
    assert_cap_cutoff(false, false);
}

#[test]
fn direct_cap_preserves_genuine_loss_before_the_cap() {
    assert_cap_cutoff(false, true);
}

#[test]
fn pending_flush_cap_ignores_live_producer_loss_before_sink_returns() {
    assert_cap_cutoff(true, false);
}

#[test]
fn pending_flush_cap_preserves_genuine_loss_before_the_cap() {
    assert_cap_cutoff(true, true);
}

struct LossDuringPushSink {
    metric: AtomicU64,
    busy_once: AtomicBool,
}

impl FrameSink for LossDuringPushSink {
    fn try_push_with_cap(&self, _frame: &[f32], on_full: &mut dyn FnMut()) -> PushOutcome {
        if self.busy_once.swap(false, Ordering::SeqCst) {
            return PushOutcome::Busy;
        }
        self.metric.fetch_add(1, Ordering::SeqCst);
        on_full();
        self.metric.fetch_add(1, Ordering::SeqCst);
        PushOutcome::RecordingFull
    }
}

#[test]
fn cap_notification_includes_loss_during_the_final_sink_call() {
    for busy_first in [false, true] {
        let sink = LossDuringPushSink {
            metric: AtomicU64::new(0),
            busy_once: AtomicBool::new(busy_first),
        };
        let mut events = [0.1, 0.2]
            .map(|value| crate::audio::PipelineEvent::Frame(vec![value]))
            .into_iter();
        let mut at_cap = None;
        forward_frames_with_report_and_cap(|| events.next(), &sink, &mut || {
            at_cap = Some(sink.metric.load(Ordering::SeqCst));
        });
        assert_eq!(at_cap, Some(1));
        assert_eq!(sink.metric.load(Ordering::SeqCst), 2);
    }
}
