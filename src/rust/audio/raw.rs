//! VAD-free push-to-talk capture pipeline (cpal → rubato resample → raw
//! frames).
//!
//! [`RawCapturePipeline`] is the sibling of [`super::AudioPipeline`] for the
//! push-to-talk path: it captures from the mic and emits every resampled
//! 30 ms / 480-sample 16 kHz frame as [`PipelineEvent::Frame`] **without**
//! running the Silero VAD. PTT bounds the utterance by the key press/release,
//! so the VAD's `SpeechStart` / `SpeechEnd` endpointing is unnecessary — the
//! in-process session's audio pump already discards those events and forwards
//! only frames. Dropping the VAD also drops the ONNX-runtime dependency, so
//! this pipeline builds under the lighter `audio-capture` feature (cpal +
//! rubato only, no `vad-rs` / `ort`) whereas [`super::AudioPipeline`] needs the
//! full `audio-in-rust` feature.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use super::capture::{self, AudioChunk, CaptureHandle};
use super::resampler::FrameResampler;
use super::PipelineEvent;

/// Running VAD-free capture pipeline. Drop or call [`Self::stop`] to tear down.
pub struct RawCapturePipeline {
    capture: Option<CaptureHandle>,
    pump: Option<JoinHandle<()>>,
}

impl RawCapturePipeline {
    /// Open the mic named `device_name` and stream resampled 16 kHz frames on
    /// the returned receiver as [`PipelineEvent::Frame`]. Capture-thread
    /// failures arrive as [`PipelineEvent::DeviceError`] (terminal — no further
    /// events follow). Unlike [`super::AudioPipeline::start`] there is no model
    /// loader: nothing is loaded before the stream opens, so `start` fails only
    /// if cpal cannot open the device.
    pub fn start(device_name: &str) -> Result<(Self, Receiver<PipelineEvent>), anyhow::Error> {
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let capture = capture::start_capture(device_name, chunk_tx)?;
        let sample_rate = capture.sample_rate() as usize;
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();
        let pump = thread::spawn(move || {
            run_raw_pump(sample_rate, chunk_rx, event_tx);
        });
        Ok((
            Self {
                capture: Some(capture),
                pump: Some(pump),
            },
            event_rx,
        ))
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
pub fn run_raw_pump(
    sample_rate: usize,
    chunk_rx: Receiver<AudioChunk>,
    event_tx: Sender<PipelineEvent>,
) {
    let mut resampler = match FrameResampler::new(sample_rate) {
        Ok(r) => r,
        Err(err) => {
            let _ = event_tx.send(PipelineEvent::DeviceError(format!(
                "construct resampler: {err}"
            )));
            return;
        }
    };

    loop {
        match chunk_rx.recv() {
            Ok(AudioChunk::Samples(samples)) => {
                let mut alive = true;
                resampler.push(&samples, |frame| {
                    if alive && event_tx.send(PipelineEvent::Frame(frame.to_vec())).is_err() {
                        alive = false;
                    }
                });
                if !alive {
                    return;
                }
            }
            Ok(AudioChunk::EndOfStream) => {
                let mut alive = true;
                resampler.finish(|frame| {
                    if alive && event_tx.send(PipelineEvent::Frame(frame.to_vec())).is_err() {
                        alive = false;
                    }
                });
                return;
            }
            Ok(AudioChunk::Error(msg)) => {
                let _ = event_tx.send(PipelineEvent::DeviceError(msg));
                return;
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Collect every event the pump emits until the sender is dropped (pump
    /// exit), with a hard deadline so a wedged pump fails instead of hanging.
    fn drain(event_rx: &Receiver<PipelineEvent>) -> Vec<PipelineEvent> {
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ev) => events.push(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
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

        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();
        chunk_tx
            .send(AudioChunk::Samples(vec![0.25; 24_000]))
            .expect("send samples");
        chunk_tx.send(AudioChunk::EndOfStream).expect("send eos");
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
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();
        chunk_tx
            .send(AudioChunk::Error("mic unplugged".to_owned()))
            .expect("send error");
        // A trailing Samples chunk that must NOT be processed after the error.
        chunk_tx
            .send(AudioChunk::Samples(vec![0.25; 24_000]))
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
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();
        // 0 Hz is not a constructible input rate for the FFT resampler.
        chunk_tx
            .send(AudioChunk::Samples(vec![0.25; 100]))
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
}
