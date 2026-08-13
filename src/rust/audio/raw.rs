//! VAD-free push-to-talk capture pipeline (cpal → rubato resample → raw
//! frames).
//!
//! [`RawCapturePipeline`] captures from the mic and emits every resampled
//! 30 ms / 480-sample 16 kHz frame as [`PipelineEvent::Frame`] **without**
//! endpoint detection. PTT key press/release already bounds the utterance.

use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::bounded_queue::{bounded_latest, LatestReceiver, LatestSender, OverflowMetric};
use super::capture::{self, AudioChunk, AudioChunkReceiver, CaptureHandle};
use super::resampler::FrameResampler;
use super::PipelineEvent;

/// Thirty-millisecond frames retained between the resampler and session pump.
/// Sixteen frames are 480 ms / 30 KiB of sample payload. A stalled consumer
/// therefore has a fixed footprint and resumes from the newest audio.
pub const PIPELINE_EVENT_QUEUE_CAPACITY: usize = 16;

const OVERFLOW_REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// Snapshot of the two monotonic overflow metrics owned by a raw pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawCaptureOverflow {
    pub capture_chunks: u64,
    pub pipeline_events: u64,
}

/// Receiving half exposed to the session and live-mic consumers.
pub struct PipelineReceiver(LatestReceiver<PipelineEvent>);

pub(crate) fn pipeline_event_channel(
    capacity: usize,
) -> (LatestSender<PipelineEvent>, PipelineReceiver) {
    let (tx, rx) = bounded_latest(capacity);
    (tx, PipelineReceiver(rx))
}

impl PipelineReceiver {
    pub fn recv(&self) -> Result<PipelineEvent, crossbeam_channel::RecvError> {
        self.0.recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<PipelineEvent, crossbeam_channel::RecvTimeoutError> {
        self.0.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<PipelineEvent, crossbeam_channel::TryRecvError> {
        self.0.try_recv()
    }
}

/// Running VAD-free capture pipeline. Drop or call [`Self::stop`] to tear down.
pub struct RawCapturePipeline {
    capture: Option<CaptureHandle>,
    pump: Option<JoinHandle<()>>,
    capture_overflow: OverflowMetric,
    event_overflow: OverflowMetric,
}

impl RawCapturePipeline {
    /// Open the mic named `device_name` and stream resampled 16 kHz frames on
    /// the returned receiver as [`PipelineEvent::Frame`]. Capture-thread
    /// failures arrive as [`PipelineEvent::DeviceError`] (terminal — no further
    /// events follow). Nothing is loaded before the stream opens, so `start`
    /// fails only if cpal cannot open the device.
    pub fn start(device_name: &str) -> Result<(Self, PipelineReceiver), anyhow::Error> {
        let (chunk_tx, chunk_rx) = capture::audio_chunk_channel();
        let capture_overflow = chunk_rx.overflow_metric();
        let capture = capture::start_capture(device_name, chunk_tx)?;
        let sample_rate = capture.sample_rate() as usize;
        let (event_tx, event_rx) = pipeline_event_channel(PIPELINE_EVENT_QUEUE_CAPACITY);
        let event_overflow = event_rx.0.overflow_metric();
        let pump = thread::spawn(move || {
            run_raw_pump(sample_rate, chunk_rx, event_tx);
        });
        Ok((
            Self {
                capture: Some(capture),
                pump: Some(pump),
                capture_overflow,
                event_overflow,
            },
            event_rx,
        ))
    }

    /// Return monotonic overflow counters for diagnostics and telemetry.
    pub fn overflow_snapshot(&self) -> RawCaptureOverflow {
        RawCaptureOverflow {
            capture_chunks: self.capture_overflow.count(),
            pipeline_events: self.event_overflow.count(),
        }
    }

    /// Stop the capture stream and join the pump thread. Idempotent.
    pub fn stop(&mut self) {
        if let Some(mut cap) = self.capture.take() {
            cap.stop();
        }
        if let Some(handle) = self.pump.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RawCapturePipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Consumer thread body: drain raw cpal bursts, resample to fixed 16 kHz
/// frames, and forward each as [`PipelineEvent::Frame`]. No VAD, so no
/// `SpeechStart` / `SpeechEnd` / `Cancelled` events are ever emitted — the
/// caller (PTT coordinator) owns the utterance lifecycle. `EndOfStream`
/// flushes the resampler tail then returns; `Error` forwards a single terminal
/// [`PipelineEvent::DeviceError`]. Exits as soon as the consumer hangs up.
/// Re-exported from the module so the unit tests can drive it directly without
/// spinning up a real cpal stream.
pub(crate) fn run_raw_pump(
    sample_rate: usize,
    chunk_rx: AudioChunkReceiver,
    event_tx: LatestSender<PipelineEvent>,
) {
    let mut capture_reporter = OverflowReporter::new("capture chunks", chunk_rx.overflow_metric());
    let mut event_reporter = OverflowReporter::new("pipeline events", event_tx.overflow_metric());
    let mut resampler = match FrameResampler::new(sample_rate) {
        Ok(r) => r,
        Err(err) => {
            send_event(
                &event_tx,
                PipelineEvent::DeviceError(format!("construct resampler: {err}")),
                &mut event_reporter,
            );
            return;
        }
    };

    loop {
        capture_reporter.observe();
        match chunk_rx.recv() {
            Ok(AudioChunk::Samples(samples)) => {
                let mut alive = true;
                resampler.push(&samples, |frame| {
                    if alive {
                        alive = send_event(
                            &event_tx,
                            PipelineEvent::Frame(frame.to_vec()),
                            &mut event_reporter,
                        );
                    }
                });
                if !alive {
                    return;
                }
            }
            Ok(AudioChunk::EndOfStream) => {
                let mut alive = true;
                resampler.finish(|frame| {
                    if alive {
                        alive = send_event(
                            &event_tx,
                            PipelineEvent::Frame(frame.to_vec()),
                            &mut event_reporter,
                        );
                    }
                });
                return;
            }
            Ok(AudioChunk::Error(msg)) => {
                send_event(
                    &event_tx,
                    PipelineEvent::DeviceError(msg),
                    &mut event_reporter,
                );
                return;
            }
            Err(_) => return,
        }
    }
}

fn send_event(
    tx: &LatestSender<PipelineEvent>,
    event: PipelineEvent,
    reporter: &mut OverflowReporter,
) -> bool {
    match tx.try_send_latest(event) {
        Ok(overflowed) => {
            if overflowed {
                reporter.observe();
            }
            true
        }
        Err(_) => false,
    }
}

/// Logs the first observed overflow immediately, then at most once per five
/// seconds. The atomic metric always records every eviction even when logging
/// is disabled or suppressed by the rate limit.
struct OverflowReporter {
    label: &'static str,
    metric: OverflowMetric,
    reported: u64,
    last_report: Option<Instant>,
}

impl OverflowReporter {
    fn new(label: &'static str, metric: OverflowMetric) -> Self {
        Self {
            label,
            metric,
            reported: 0,
            last_report: None,
        }
    }

    fn observe(&mut self) {
        let total = self.metric.count();
        if total == self.reported || !crate::diag::debug_enabled() {
            return;
        }
        let now = Instant::now();
        if self
            .last_report
            .is_some_and(|last| now.duration_since(last) < OVERFLOW_REPORT_INTERVAL)
        {
            return;
        }
        crate::diag::log!(
            "[audio/raw] bounded queue overflow: dropped {} oldest {} (total {})",
            total - self.reported,
            self.label,
            total
        );
        self.reported = total;
        self.last_report = Some(now);
    }
}

impl Drop for OverflowReporter {
    fn drop(&mut self) {
        let total = self.metric.count();
        if total > self.reported && crate::diag::debug_enabled() {
            crate::diag::log!(
                "[audio/raw] bounded queue overflow: dropped {} oldest {} (total {})",
                total - self.reported,
                self.label,
                total
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Collect every event the pump emits until the sender is dropped (pump
    /// exit), with a hard deadline so a wedged pump fails instead of hanging.
    fn drain(event_rx: &LatestReceiver<PipelineEvent>) -> Vec<PipelineEvent> {
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ev) => events.push(ev),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        events
    }

    /// The VAD-free pump emits exactly one `Frame` per resampled 16 kHz frame
    /// (push burst + EndOfStream flush) and nothing else — no `SpeechStart` /
    /// `SpeechEnd`, proving the endpointing is genuinely absent.
    #[test]
    fn raw_pump_emits_one_frame_per_resampled_frame_and_no_vad_events() {
        // Expected frame count for this input, computed straight from the
        // resampler so the assertion tracks the real framing math.
        let expected = {
            let mut r = FrameResampler::new(48_000).expect("resampler");
            let mut total = 0usize;
            r.push(&vec![0.25; 24_000], |_| total += 1);
            r.finish(|_| total += 1);
            total
        };
        assert!(expected > 0, "test setup must produce at least one frame");

        let (chunk_tx, chunk_rx) = capture::audio_chunk_channel();
        let (event_tx, event_rx) = bounded_latest(expected + 1);
        chunk_tx
            .try_send_latest(AudioChunk::Samples(vec![0.25; 24_000]))
            .expect("send samples");
        chunk_tx
            .try_send_latest(AudioChunk::EndOfStream)
            .expect("send eos");
        drop(chunk_tx);
        let handle = thread::spawn(move || run_raw_pump(48_000, chunk_rx, event_tx));

        let events = drain(&event_rx);
        let _ = handle.join();

        let frames = events
            .iter()
            .filter(|e| matches!(e, PipelineEvent::Frame(_)))
            .count();
        assert_eq!(frames, expected, "one Frame per resampled frame");
        for ev in &events {
            match ev {
                PipelineEvent::Frame(f) => assert_eq!(f.len(), super::super::FRAME_SIZE),
                other => panic!("VAD-free pump must emit only Frame events, got {other:?}"),
            }
        }
    }

    /// An `Error` chunk becomes a single terminal `DeviceError`; the pump then
    /// stops (no further events), matching the wire contract.
    #[test]
    fn raw_pump_forwards_device_error_and_stops() {
        let (chunk_tx, chunk_rx) = capture::audio_chunk_channel();
        let (event_tx, event_rx) = bounded_latest(4);
        chunk_tx
            .try_send_latest(AudioChunk::Error("mic unplugged".to_owned()))
            .expect("send error");
        // A trailing Samples chunk that must NOT be processed after the error.
        chunk_tx
            .try_send_latest(AudioChunk::Samples(vec![0.25; 24_000]))
            .expect("send trailing samples");
        drop(chunk_tx);
        let handle = thread::spawn(move || run_raw_pump(48_000, chunk_rx, event_tx));

        let events = drain(&event_rx);
        let _ = handle.join();

        assert_eq!(
            events,
            vec![PipelineEvent::DeviceError("mic unplugged".to_owned())],
            "exactly one DeviceError, nothing after it",
        );
    }

    /// A sample rate the resampler cannot construct for surfaces as a
    /// `DeviceError` rather than a panic in the pump thread.
    #[test]
    fn raw_pump_reports_resampler_construction_failure() {
        let (chunk_tx, chunk_rx) = capture::audio_chunk_channel();
        let (event_tx, event_rx) = bounded_latest(2);
        // 0 Hz is not a constructible input rate for the FFT resampler.
        chunk_tx
            .try_send_latest(AudioChunk::Samples(vec![0.25; 100]))
            .expect("send samples");
        drop(chunk_tx);
        let handle = thread::spawn(move || run_raw_pump(0, chunk_rx, event_tx));

        let events = drain(&event_rx);
        let _ = handle.join();

        assert!(
            matches!(events.as_slice(), [PipelineEvent::DeviceError(msg)] if msg.contains("resampler")),
            "resampler construction failure must be a single DeviceError, got {events:?}",
        );
    }

    #[test]
    fn full_output_queue_does_not_delay_pump_shutdown() {
        let (chunk_tx, chunk_rx) = capture::audio_chunk_channel();
        let (event_tx, event_rx) = bounded_latest(1);
        for value in [0.1, 0.2, 0.3] {
            chunk_tx
                .try_send_latest(AudioChunk::Samples(vec![value; 24_000]))
                .expect("queue samples");
        }
        chunk_tx
            .try_send_latest(AudioChunk::EndOfStream)
            .expect("queue eos");
        drop(chunk_tx);

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            run_raw_pump(48_000, chunk_rx, event_tx);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("a saturated downstream queue must not delay shutdown");
        assert!(
            event_rx.overflow_metric().count() > 0,
            "the fixture must saturate the output queue"
        );
        assert_eq!(event_rx.len(), 1);
    }
}
