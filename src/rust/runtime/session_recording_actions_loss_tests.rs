//! The real capture -> recording action -> utterance diagnostic handoff.

use super::{env_lock, wait_until, NoopInject, SR};
use crate::audio::PipelineEvent;
use crate::dictate::session::audio_loss::RecordingAudioLoss;
use crate::dictate::{
    DictateSession, SessionConfig, TranscribeBackend, TranscribeError, TranscribeResult,
};
use crate::hotkey::coordinator::CoordinatorAction;
use crate::runtime::capture_forwarder::{FrameSink, PushOutcome, SessionFrameSink};
use crate::runtime::capture_lifecycle::CaptureLifecycle;
use crate::runtime::capture_test_support::{lifecycle_with, FakeOpener, ReporterRig};
use crate::runtime::live_settings::LiveEnvOverrides;
use crate::runtime::recording_capture::{RecordingCapture, RecordingCaptureHandle};
use crate::runtime::rust_session_sink::{
    build_session_action_sink_with_live_overrides, CoordinatorSignals,
};
use crate::runtime::RuntimeEvent;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

struct TextTranscribe;
impl TranscribeBackend for TextTranscribe {
    fn transcribe(&self, _: &[f32], _: u32) -> Result<TranscribeResult, TranscribeError> {
        Ok(TranscribeResult {
            text: "captured words".to_owned(),
            ..Default::default()
        })
    }
}

type Session = DictateSession<TextTranscribe, NoopInject>;

struct StalledSink {
    inner: SessionFrameSink<TextTranscribe, NoopInject>,
    busy: AtomicBool,
    accepted: AtomicUsize,
}
impl FrameSink for StalledSink {
    fn try_push_with_cap(&self, frame: &[f32], on_full: &mut dyn FnMut()) -> PushOutcome {
        if self.busy.load(Ordering::SeqCst) {
            return PushOutcome::Busy;
        }
        let outcome = self.inner.try_push_with_cap(frame, on_full);
        if outcome != PushOutcome::Busy {
            self.accepted.fetch_add(1, Ordering::SeqCst);
        }
        outcome
    }
}

struct Fixture {
    opener: FakeOpener,
    session: Arc<Mutex<Session>>,
    sink: Arc<StalledSink>,
    capture: Arc<CaptureLifecycle<FakeOpener, StalledSink>>,
    reports: ReporterRig,
}

impl Fixture {
    fn new(max_record_seconds: Option<f64>) -> Self {
        let opener = FakeOpener::default();
        let session = Arc::new(Mutex::new(
            DictateSession::new(
                TextTranscribe,
                NoopInject,
                SessionConfig {
                    max_record_seconds,
                    ..Default::default()
                },
            )
            .with_worker_events_enabled(),
        ));
        let sink = Arc::new(StalledSink {
            inner: SessionFrameSink::new(Arc::clone(&session)),
            busy: AtomicBool::new(false),
            accepted: AtomicUsize::new(0),
        });
        let (capture, reports) = lifecycle_with(&opener, Arc::clone(&sink), "USB mic");
        Self {
            opener,
            session,
            sink,
            capture: Arc::new(capture),
            reports,
        }
    }

    fn actions(&self) -> (impl FnMut(CoordinatorAction), mpsc::Receiver<RuntimeEvent>) {
        let (tx, rx) = mpsc::channel();
        let capture: RecordingCaptureHandle = self.capture.clone();
        let actions = build_session_action_sink_with_live_overrides(
            Arc::clone(&self.session),
            tx,
            CoordinatorSignals {
                processing_finished: |_| {},
                recording_abandoned: |_| {},
            },
            None,
            LiveEnvOverrides::default(),
            false,
            Some(capture),
        );
        (actions, rx)
    }

    fn lose_pending_frames(&self) {
        self.sink.busy.store(true, Ordering::SeqCst);
        for _ in 0..450 {
            assert!(self.opener.feed(PipelineEvent::Frame(vec![0.5])));
        }
        // Close joins while delivery is stalled; eviction + final discard sum
        // to exactly 450, independent of the forwarder's scheduling.
        self.capture.close_for_recording();
        self.sink.busy.store(false, Ordering::SeqCst);
    }
}

fn utterances(events: &mpsc::Receiver<RuntimeEvent>) -> Vec<serde_json::Value> {
    events
        .try_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Worker(worker) if worker.event == "utterance" => Some(worker.payload),
            _ => None,
        })
        .collect()
}

fn assert_counts(payload: &serde_json::Value, loss: RecordingAudioLoss) {
    for (key, count) in loss.fields() {
        assert_eq!(payload[key].as_u64(), Some(count), "{key}");
    }
}

#[test]
fn lossy_capture_is_attached_to_its_utterance_but_not_the_next_one() {
    let _env = env_lock();
    let fixture = Fixture::new(None);
    let (mut actions, events) = fixture.actions();
    actions(CoordinatorAction::StartRecording(1));
    assert!(fixture.opener.feed(PipelineEvent::Frame(vec![0.1; SR])));
    wait_until("initial audio reaches the session", || {
        fixture.sink.accepted.load(Ordering::SeqCst) == 1
    });
    fixture.lose_pending_frames();
    actions(CoordinatorAction::StopAndTranscribe(1));
    let first = utterances(&events);
    assert_eq!(first.len(), 1);
    assert_counts(
        &first[0],
        RecordingAudioLoss {
            pending_frames_dropped: 450,
            ..Default::default()
        },
    );
    assert!(
        fixture.capture.take_recording_loss().is_none(),
        "handoff consumes the snapshot"
    );
    assert_eq!(fixture.reports.drain().iter().filter(|event| matches!(event, RuntimeEvent::Worker(worker) if worker.event == "audio_overflow")).count(), 1);

    actions(CoordinatorAction::StartRecording(2));
    assert!(fixture.opener.feed(PipelineEvent::Frame(vec![0.1; SR])));
    actions(CoordinatorAction::StopAndTranscribe(2));
    let second = utterances(&events);
    assert_eq!(second.len(), 1);
    assert_counts(&second[0], RecordingAudioLoss::default());
    assert!(fixture.reports.drain().is_empty());
}

#[test]
fn cancelled_loss_is_discarded_before_a_healthy_utterance() {
    let _env = env_lock();
    let fixture = Fixture::new(None);
    let (mut actions, events) = fixture.actions();
    actions(CoordinatorAction::StartRecording(1));
    fixture.lose_pending_frames();
    actions(CoordinatorAction::CancelRecording(1));
    assert!(utterances(&events).is_empty());
    assert!(fixture.capture.take_recording_loss().is_none());
    fixture.reports.drain();
    actions(CoordinatorAction::StartRecording(2));
    assert!(fixture.opener.feed(PipelineEvent::Frame(vec![0.1; SR])));
    actions(CoordinatorAction::StopAndTranscribe(2));
    let payloads = utterances(&events);
    assert_eq!(payloads.len(), 1);
    assert_counts(&payloads[0], RecordingAudioLoss::default());
    assert!(fixture.reports.drain().is_empty());
}

#[test]
fn intentional_recording_cap_discard_has_zero_loss_in_the_utterance() {
    let _env = env_lock();
    let fixture = Fixture::new(Some(1.0));
    let (mut actions, events) = fixture.actions();
    actions(CoordinatorAction::StartRecording(1));
    assert!(fixture.opener.feed(PipelineEvent::Frame(vec![0.1; 2 * SR])));
    wait_until("recording cap closes capture", || {
        fixture.opener.open_streams() == 0
    });
    actions(CoordinatorAction::StopAndTranscribe(1));
    let payloads = utterances(&events);
    assert_eq!(payloads.len(), 1);
    assert_counts(&payloads[0], RecordingAudioLoss::default());
    let reports = fixture.reports.drain();
    assert!(reports.iter().any(|event| matches!(event,
        RuntimeEvent::Stderr(line) if line.contains("max_record_s"))));
    assert!(reports.iter().all(|event| match event {
        RuntimeEvent::Worker(worker) => worker.event != "audio_overflow",
        RuntimeEvent::Stderr(line) => !line.contains("transcript may be incomplete"),
        _ => true,
    }));
}

struct MeteredOpener {
    opened: AtomicBool,
}

struct MeteredStream {
    _producer: crate::audio::bounded_queue::LatestSender<PipelineEvent>,
}

impl crate::runtime::capture_lifecycle::CaptureOpener for MeteredOpener {
    type Stream = MeteredStream;

    fn probe(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn open(&self, _: &str) -> anyhow::Result<(Self::Stream, crate::audio::PipelineReceiver)> {
        use crate::audio::capture::{audio_chunk_channel, AudioChunk, CAPTURE_QUEUE_CAPACITY};
        use crate::audio::raw::pipeline_event_channel_with_capture_metric_for_test;

        let lossy = !self.opened.swap(true, Ordering::SeqCst);
        let (chunks, chunk_receiver) = audio_chunk_channel();
        let chunk_count = if lossy { CAPTURE_QUEUE_CAPACITY + 2 } else { 1 };
        for _ in 0..chunk_count {
            chunks
                .try_send_latest(AudioChunk::Samples(vec![0.1]))
                .unwrap();
        }
        let metric = chunk_receiver.overflow_metric();
        let (producer, receiver) = pipeline_event_channel_with_capture_metric_for_test(2, metric);
        for _ in 0..if lossy { 3 } else { 2 } {
            producer
                .try_send_latest(PipelineEvent::Frame(vec![0.1; SR]))
                .unwrap();
        }
        // Both genuine queue evictions happen before the consumer is spawned,
        // so their exact counts do not depend on thread scheduling.
        Ok((
            MeteredStream {
                _producer: producer,
            },
            receiver,
        ))
    }

    fn is_timeout(&self, _: &anyhow::Error) -> bool {
        false
    }
}

#[test]
fn raw_queue_counts_cross_the_action_boundary_into_utterance_and_disk_rows() {
    use crate::dictate::session::{JsonlHistorySink, JsonlMetricsSink};
    use crate::runtime::capture_status::CaptureReporter;
    use std::sync::RwLock;

    let _env = env_lock();
    let dir = tempfile::tempdir().unwrap();
    let history = dir.path().join("history.jsonl");
    let metrics = dir.path().join("metrics.jsonl");
    let session = Arc::new(Mutex::new(
        DictateSession::new(TextTranscribe, NoopInject, SessionConfig::default())
            .with_worker_events_enabled()
            .with_history_sink(Box::new(JsonlHistorySink::new(history.clone())))
            .with_metrics_sink(Box::new(JsonlMetricsSink::new(metrics.clone()))),
    ));
    let frames = Arc::new(SessionFrameSink::new(Arc::clone(&session)));
    let (report_tx, reports) = mpsc::channel();
    let reporter = CaptureReporter::new(report_tx, None, Arc::new(RwLock::new(String::new())));
    let capture: RecordingCaptureHandle = Arc::new(CaptureLifecycle::new(
        MeteredOpener {
            opened: AtomicBool::new(false),
        },
        frames,
        "",
        reporter,
    ));
    let (tx, events) = mpsc::channel();
    let mut actions = build_session_action_sink_with_live_overrides(
        session,
        tx,
        CoordinatorSignals {
            processing_finished: |_| {},
            recording_abandoned: |_| {},
        },
        None,
        LiveEnvOverrides::default(),
        false,
        Some(capture),
    );
    let mut payloads = Vec::new();
    for id in [1, 2] {
        actions(CoordinatorAction::StartRecording(id));
        actions(CoordinatorAction::StopAndTranscribe(id));
        payloads.extend(utterances(&events));
    }
    assert_eq!(payloads.len(), 2);
    let loss = RecordingAudioLoss {
        capture_chunks_dropped: 2,
        pipeline_events_dropped: 1,
        pending_frames_dropped: 0,
    };
    assert_counts(&payloads[0], loss);
    assert_counts(&payloads[1], RecordingAudioLoss::default());
    for path in [history, metrics] {
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_counts(&rows[0], loss);
        assert_counts(&rows[1], RecordingAudioLoss::default());
    }
    let reports: Vec<_> = reports.try_iter().collect();
    assert_eq!(
        reports
            .iter()
            .filter(|event| matches!(event,
        RuntimeEvent::Worker(worker) if worker.event == "audio_overflow"))
            .count(),
        1
    );
}
