//! Cap teardown drives the real resampler without opening an audio device.

use crate::audio::bounded_queue::{LatestSender, OverflowMetric};
use crate::audio::capture::{audio_chunk_channel, AudioChunk, AudioChunkReceiver};
use crate::audio::raw::{pipeline_event_channel, run_raw_pump, PIPELINE_EVENT_QUEUE_CAPACITY};
use crate::audio::{PipelineEvent, PipelineReceiver};
use crate::runtime::capture_forwarder::{FrameSink, PushOutcome};
use crate::runtime::capture_lifecycle::{CaptureLifecycle, CaptureOpener};
use crate::runtime::capture_status::CaptureReporter;
use crate::runtime::capture_test_support::{
    stderr_lines, wait_until, RecordingFrames, ReporterRig,
};
use crate::runtime::recording_capture::RecordingCapture;
use crate::runtime::RuntimeEvent;
use std::sync::{mpsc, Arc, Mutex, RwLock};

struct ReadyCapSink {
    frames: Arc<RecordingFrames>,
    ready: Mutex<Option<mpsc::Receiver<()>>>,
}

impl FrameSink for ReadyCapSink {
    fn try_push_with_cap(&self, frame: &[f32], on_full: &mut dyn FnMut()) -> PushOutcome {
        if let Some(ready) = self.ready.lock().unwrap().take() {
            ready
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
        }
        self.frames.try_push_with_cap(frame, on_full)
    }
}

struct BufferedCapOpener {
    metric: Arc<std::sync::Mutex<Option<OverflowMetric>>>,
}

struct BufferedCapStream {
    chunks: Option<AudioChunkReceiver>,
    events: Option<LatestSender<PipelineEvent>>,
}

impl Drop for BufferedCapStream {
    fn drop(&mut self) {
        // Same real pump/resampler drain performed by RawCapturePipeline.stop.
        // Delay it until stream drop so no post-cap chunk is raced into capture.
        run_raw_pump(
            48_000,
            self.chunks.take().unwrap(),
            self.events.take().unwrap(),
        );
    }
}

impl CaptureOpener for BufferedCapOpener {
    type Stream = BufferedCapStream;

    fn probe(&self, _selector: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn open(&self, _selector: &str) -> anyhow::Result<(Self::Stream, PipelineReceiver)> {
        let (chunks, chunk_rx) = audio_chunk_channel();
        for sample in [0.1, 0.2, 0.3] {
            assert!(!chunks
                .try_send_latest(AudioChunk::Samples(vec![sample; 24_000]))
                .unwrap());
        }
        assert!(!chunks.try_send_latest(AudioChunk::EndOfStream).unwrap());
        drop(chunks);
        let (events, receiver) = pipeline_event_channel(PIPELINE_EVENT_QUEUE_CAPACITY);
        *self.metric.lock().unwrap() = Some(events.overflow_metric());
        assert!(!events
            .try_send_latest(PipelineEvent::Frame(vec![0.5; 480]))
            .unwrap());
        assert_eq!(
            receiver.overflow_snapshot(),
            crate::audio::RawCaptureOverflow::default()
        );
        Ok((
            BufferedCapStream {
                chunks: Some(chunk_rx),
                events: Some(events),
            },
            receiver,
        ))
    }

    fn is_timeout(&self, _error: &anyhow::Error) -> bool {
        false
    }
}

#[test]
fn cap_only_resampler_teardown_does_not_report_recording_overflow() {
    let frames = Arc::new(RecordingFrames::default());
    frames.set_capacity(1);
    let metric = Arc::new(std::sync::Mutex::new(None));
    let opener = BufferedCapOpener {
        metric: Arc::clone(&metric),
    };
    let (tx, rx) = mpsc::channel();
    let effective_device = Arc::new(RwLock::new(String::new()));
    let reporter = CaptureReporter::new(tx, None, Arc::clone(&effective_device));
    let (ready_tx, ready_rx) = mpsc::channel();
    let sink = Arc::new(ReadyCapSink {
        frames: Arc::clone(&frames),
        ready: Mutex::new(Some(ready_rx)),
    });
    let lifecycle = CaptureLifecycle::new(opener, sink, "", reporter);
    let rig = ReporterRig {
        rx,
        effective_device,
    };
    assert!(lifecycle.open_for_recording());
    ready_tx.send(()).unwrap();
    wait_until("cap drains the real buffered pipeline", || {
        metric
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|value| value.count() > 0)
    });
    lifecycle.close_for_recording();
    let events = rig.drain();
    assert!(
        metric.lock().unwrap().as_ref().unwrap().count() > 0,
        "the real post-cap resampler must saturate the abandoned event queue"
    );
    assert_eq!(frames.frames().len(), 1);
    assert!(stderr_lines(&events)
        .iter()
        .any(|line| line.contains("max_record_s")));
    assert!(!stderr_lines(&events)
        .iter()
        .any(|line| line.contains("incomplete")));
    assert!(!events.iter().any(|event| matches!(event,
        RuntimeEvent::Worker(worker) if worker.event == "audio_overflow")));
}
