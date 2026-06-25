//! Rust-side audio capture pipeline (cpal → resampler → Silero VAD).
//!
//! Gated end-to-end behind the `audio-in-rust` cargo feature (off by
//! default). When the feature is off, the resampler / mix-to-mono / VAD
//! state-machine code still compiles + unit-tests so the rest of the app
//! is unaffected; only the cpal stream + Silero ONNX model are gated out.
//!
//! Layered design:
//! 1. [`capture`] runs cpal in a worker thread and emits raw `f32` mono
//!    samples at the device's native rate via an `mpsc::channel`.
//! 2. [`resampler`] consumes those bursts and emits fixed 30 ms / 480-
//!    sample frames at 16 kHz.
//! 3. [`vad`] consumes the 16 kHz frames and emits `SpeechStart` /
//!    `SpeechFrame` / `SpeechEnd` events with prefill + onset debounce +
//!    hangover smoothing.
//!
//! [`AudioPipeline`] wires the three together on a single consumer
//! thread (the cpal callback stays minimal) and re-emits the events on a
//! single `mpsc::Receiver<PipelineEvent>` that the supervisor pipes to
//! the Python worker's stdin.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

pub mod capture;
pub(crate) mod model_cache;
pub mod pipe;
pub mod resampler;
pub mod vad;

pub use capture::{AudioChunk, CaptureHandle};
pub use pipe::{event_to_json_line, write_events};
pub use resampler::{FrameResampler, FRAME_SIZE, OUTPUT_RATE};
pub use vad::{SileroVad, SmoothedVad, VadEvent};

/// Events emitted by the assembled pipeline. Mirrors the JSON message
/// vocabulary the Python `vp_capture` rust-stdin reader expects, so the
/// supervisor's serializer is a one-line transformation per event.
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineEvent {
    /// One 30 ms / 480-sample frame at 16 kHz, sent every speech frame
    /// (both real voice + hangover silence inside an utterance, AND the
    /// frames in the onset burst). The Python side reassembles them into
    /// an utterance buffer.
    Frame(Vec<f32>),
    /// Speech onset. The first such frame is always preceded by a
    /// `SpeechStart` event so the Python side can begin a new utterance.
    SpeechStart,
    /// Speech ended (hangover exhausted). The Python side flushes the
    /// utterance buffer to the transcriber.
    SpeechEnd,
    /// Capture failed unrecoverably. The supervisor should surface this
    /// to the user; the pipeline thread exits after sending this. This is
    /// the terminal event on the wire — no further messages will arrive.
    DeviceError(String),
    /// The current utterance was cancelled mid-flight (e.g. the user
    /// released the PTT key, or a `reset()` was issued while
    /// `in_speech == true`). Emitted INSTEAD of `SpeechEnd` when there is
    /// no commitable utterance — the Python side should drop its buffer
    /// rather than flush it to the transcriber.
    ///
    /// Wire contract: `Cancelled` is emitted strictly between
    /// `SpeechStart` and any would-be `SpeechEnd`; the consumer must
    /// treat it as "discard current utterance" and return to the
    /// pre-speech state.
    Cancelled,
}

/// Running pipeline. Drop or call [`AudioPipeline::stop`] to tear down.
pub struct AudioPipeline {
    capture: Option<CaptureHandle>,
    pump: Option<JoinHandle<()>>,
}

impl AudioPipeline {
    /// Spin up the full pipeline. Errors from the capture thread are
    /// reported on the `Receiver` as [`PipelineEvent::DeviceError`].
    ///
    /// `model_loader` builds the [`SileroVad`] — split out so callers can
    /// either embed the ONNX bytes or load from disk without making this
    /// module aware of the choice.
    pub fn start<L>(
        device_name: &str,
        model_loader: L,
    ) -> Result<(Self, Receiver<PipelineEvent>), anyhow::Error>
    where
        L: FnOnce() -> Result<SileroVad, anyhow::Error>,
    {
        // Iteration-2 review finding #4: load the Silero ONNX model
        // FIRST, BEFORE we open the cpal capture stream. ORT
        // initialisation can take 200-500 ms on cold cache (and longer
        // when AV scans the model file), during which the cpal
        // callback would otherwise be queueing pre-pipeline-ready
        // chunks into an unbounded mpsc — those stale samples would
        // then be fed into the VAD once the pump started, potentially
        // emitting speech events for audio captured BEFORE the
        // pipeline was ready. Worse, a model-load failure left the
        // user with a mic-permission prompt + an open stream for
        // nothing. By loading first we get fail-fast behaviour: no
        // stream is ever opened on load failure.
        let silero = model_loader()?;
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let capture = capture::start_capture(device_name, chunk_tx)?;
        let sample_rate = capture.sample_rate() as usize;
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();
        let pump = thread::spawn(move || {
            run_pump(sample_rate, silero, chunk_rx, event_tx);
        });
        Ok((
            Self {
                capture: Some(capture),
                pump: Some(pump),
            },
            event_rx,
        ))
    }

    pub fn stop(&mut self) {
        if let Some(mut cap) = self.capture.take() {
            cap.stop();
        }
        if let Some(handle) = self.pump.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Consumer thread: reads raw bursts from cpal, runs them through the
/// resampler + VAD, and forwards [`PipelineEvent`]s. `pub(crate)` so the
/// regression test for the DeviceError terminal-event contract can
/// drive it directly without spinning up a real cpal stream.
pub(crate) fn run_pump(
    sample_rate: usize,
    silero: SileroVad,
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
    let mut vad_state = SmoothedVad::new(silero);

    // Helper: push a 30 ms frame through the VAD and translate the
    // result into PipelineEvents. Returns false if the consumer hung up.
    let mut on_frame = |frame: &[f32], event_tx: &Sender<PipelineEvent>| -> bool {
        match vad_state.feed(frame) {
            Ok(VadEvent::Silence) => true,
            Ok(VadEvent::SpeechStart(burst)) => {
                if event_tx.send(PipelineEvent::SpeechStart).is_err() {
                    return false;
                }
                for f in burst {
                    if event_tx.send(PipelineEvent::Frame(f)).is_err() {
                        return false;
                    }
                }
                true
            }
            Ok(VadEvent::SpeechFrame(f)) => event_tx.send(PipelineEvent::Frame(f)).is_ok(),
            Ok(VadEvent::SpeechEnd) => event_tx.send(PipelineEvent::SpeechEnd).is_ok(),
            Err(err) => {
                // DeviceError is the documented terminal event on the wire
                // contract: "no further messages will arrive after
                // device_error" (see vp_rust_audio_source.py module doc).
                // We MUST stop the pump unconditionally — success of the
                // send is irrelevant; even if the consumer hung up we still
                // shut down. Returning `false` here cascades up through
                // `alive` in run_pump and exits the loop.
                let _ = event_tx.send(PipelineEvent::DeviceError(format!("vad: {err}")));
                false
            }
        }
    };

    loop {
        match chunk_rx.recv() {
            Ok(AudioChunk::Samples(samples)) => {
                let mut alive = true;
                resampler.push(&samples, |frame| {
                    if alive {
                        alive = on_frame(frame, &event_tx);
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
                        alive = on_frame(frame, &event_tx);
                    }
                });
                // Iteration-2 review finding #1: if the flush above tripped
                // a VAD error, `alive` is now `false` because `on_frame`
                // already emitted the terminal `DeviceError`. The wire
                // contract documents `DeviceError` as terminal — "no
                // further messages will arrive" (see
                // vp_rust_audio_source.py module doc) — so we MUST NOT
                // tack on a final `SpeechEnd` here. The Python decoder
                // would otherwise treat that EOS-shaped event as a
                // legitimate flush of an utterance whose audio errored
                // mid-way.
                if alive && vad_state.in_speech() {
                    let _ = event_tx.send(PipelineEvent::SpeechEnd);
                }
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
    use crate::audio::capture::AudioChunk;
    use std::time::Duration;

    /// Regression test for review finding #1: when the VAD returns an
    /// error, the pump MUST emit exactly one DeviceError and then stop
    /// — no further Frame / SpeechStart / SpeechEnd events. The
    /// previous closure returned `send().is_ok()` which evaluated to
    /// `true` on success, leaving the pump live and re-entering the VAD
    /// on every subsequent chunk (= a flood of DeviceError messages and
    /// possibly Frames if the error recovered, which broke the
    /// documented "no further messages after device_error" contract on
    /// vp_rust_audio_source.py).
    #[test]
    fn vad_error_emits_single_device_error_and_stops_pump() {
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();

        // Drive the pump with audio at a rate the resampler can
        // construct. Use 48 kHz so we don't trip rubato's small-rate
        // checks; the values don't matter because the VAD errors out
        // on the first frame regardless.
        let silero = SileroVad::always_error_for_tests();
        // Send three full chunks BEFORE starting the pump so they're
        // queued — if the pump (buggy version) kept going past the
        // first error it would consume the second + third and emit more
        // DeviceErrors.
        // 1 second of audio per chunk at 48 kHz = enough to push many
        // 480-sample 16 kHz frames through the resampler.
        let one_second: Vec<f32> = vec![0.5; 48_000];
        for _ in 0..3 {
            chunk_tx
                .send(AudioChunk::Samples(one_second.clone()))
                .expect("send chunk");
        }
        // Don't drop chunk_tx yet — we want to verify the pump STOPS
        // even while the producer is still alive.
        let handle = std::thread::spawn(move || {
            run_pump(48_000, silero, chunk_rx, event_tx);
        });

        // Collect all events the pump emits, with a short timeout so a
        // never-exiting pump fails the test rather than hanging CI.
        let mut events: Vec<PipelineEvent> = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ev) => events.push(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // If the pump has exited the Sender is dropped and
                    // the next recv will return Disconnected; otherwise
                    // it's still alive — keep waiting up to the deadline.
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // Producer is intentionally still alive; drop it now so the
        // join below doesn't accidentally block on a still-running
        // pump that is waiting on us.
        drop(chunk_tx);
        let _ = handle.join();

        // Contract: the FIRST event must be a DeviceError and there
        // must be NO subsequent Frame / SpeechStart / SpeechEnd events.
        // Multiple DeviceErrors would also violate the contract.
        assert!(
            !events.is_empty(),
            "pump must emit at least one event (the terminal DeviceError)"
        );
        let device_errors = events
            .iter()
            .filter(|e| matches!(e, PipelineEvent::DeviceError(_)))
            .count();
        assert_eq!(
            device_errors, 1,
            "pump must emit exactly one DeviceError on VAD failure, got events: {events:?}",
        );
        for ev in &events {
            match ev {
                PipelineEvent::DeviceError(_) => {}
                other => panic!(
                    "no events allowed after DeviceError, got {other:?} in events: {events:?}",
                ),
            }
        }
    }

    /// Regression test for iteration-2 review finding #4: the Silero
    /// model loader must run BEFORE `start_capture`. If we open the
    /// stream first and only then load the model, a slow (or failing)
    /// ORT init leaves the cpal callback enqueuing stale pre-ready
    /// chunks (and a failed load wastes the user's mic-permission
    /// prompt). This test sets a flag inside `start_capture` via a
    /// hook (we can't substitute cpal in-process) and asserts the
    /// model-loader closure already ran by the time `start_capture`
    /// would be invoked — by making the loader fail and observing
    /// that the device-name we pass (which would otherwise be
    /// looked up via cpal) is never inspected.
    #[test]
    fn start_returns_loader_error_without_opening_capture_stream() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // The loader closure flips this flag when it runs. If we ever
        // reach start_capture, the device-name string would surface
        // in the error message (cpal returns "device not found: foo"
        // or similar for an unknown device). We use a device name
        // that's guaranteed not to exist on any host so an accidental
        // cpal call produces a distinctive, easy-to-grep error.
        static LOADER_RAN: AtomicBool = AtomicBool::new(false);
        LOADER_RAN.store(false, Ordering::SeqCst);
        let result = AudioPipeline::start("__nonexistent_device_finding_4_test__", || {
            LOADER_RAN.store(true, Ordering::SeqCst);
            Err(anyhow::anyhow!(
                "synthetic model-load failure for finding-4 test"
            ))
        });
        assert!(
            LOADER_RAN.load(Ordering::SeqCst),
            "model loader must run during start()",
        );
        let err = result
            .err()
            .expect("start must return Err when loader fails");
        let msg = format!("{err:#}");
        // The error must be our synthetic loader error, NOT a cpal
        // device-lookup error referencing the device name — proving
        // `start_capture` was not called.
        assert!(
            msg.contains("synthetic model-load failure for finding-4 test"),
            "expected loader error, got: {msg}",
        );
        assert!(
            !msg.contains("__nonexistent_device_finding_4_test__"),
            "device name appeared in error, implying start_capture ran \
             AFTER the loader failure (it must run BEFORE): {msg}",
        );
    }

    /// Regression test for iteration-2 review finding #1: when the VAD
    /// errors out during the EndOfStream flush AFTER the pump entered
    /// `in_speech == true`, the previous code emitted a final
    /// `SpeechEnd` because the `if vad_state.in_speech()` check ignored
    /// the `alive` flag. The wire contract is that `DeviceError` is
    /// terminal — no further events (including `SpeechEnd`) may follow.
    /// This test:
    ///   1. Drives the pump with above-threshold "voice" frames until
    ///      `SpeechStart` fires (i.e. `in_speech == true`).
    ///   2. Sends `EndOfStream`. The flush emits more 16 kHz frames; the
    ///      `ErrorAfter` backend has burned through its success budget,
    ///      so the first flushed frame errors → `DeviceError`, `alive`
    ///      is false.
    ///   3. Asserts that NO `SpeechEnd` follows the `DeviceError`.
    #[test]
    fn vad_error_during_eos_flush_does_not_emit_speech_end() {
        // First, count how many frames a "Samples then EOS" pair produces
        // when the VAD never errors. This is the budget we need to size
        // the error trigger against. We use the RMS stub so frames are
        // always above-threshold (loud sine surrogate via constant 0.5).
        let frames_per_run = {
            use crate::audio::resampler::FrameResampler;
            let mut r = FrameResampler::new(48_000).expect("resampler");
            let mut total = 0usize;
            r.push(&vec![0.5; 12_000], |_| total += 1);
            let frames_after_push = total;
            r.finish(|_| total += 1);
            let frames_after_flush = total - frames_after_push;
            (frames_after_push, frames_after_flush)
        };
        let (chunk_push, chunk_flush) = frames_per_run;
        // Sanity: the push must give us at least ONSET_FRAMES (=2) so
        // in_speech becomes true BEFORE EOS, AND the flush must emit at
        // least one frame so the EOS-branch resampler.finish callback
        // actually runs the VAD (i.e. exercises the bug-prone branch).
        assert!(
            chunk_push >= 2,
            "test setup: push must produce >=2 frames to enter in_speech; got {chunk_push}",
        );
        assert!(
            chunk_flush >= 1,
            "test setup: flush must produce >=1 frame to invoke the EOS-branch VAD; got {chunk_flush}",
        );

        // Now drive the real pump with an error budget = chunk_push,
        // so all push frames succeed (in_speech becomes true) and the
        // FIRST flush frame in the EOS branch errors.
        let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
        let (event_tx, event_rx) = mpsc::channel::<PipelineEvent>();
        let silero = SileroVad::error_after_for_tests(chunk_push);
        chunk_tx
            .send(AudioChunk::Samples(vec![0.5; 12_000]))
            .expect("send samples");
        chunk_tx.send(AudioChunk::EndOfStream).expect("send eos");
        drop(chunk_tx);
        let handle = std::thread::spawn(move || {
            run_pump(48_000, silero, chunk_rx, event_tx);
        });
        let mut events: Vec<PipelineEvent> = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ev) => events.push(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = handle.join();

        // Sanity: SpeechStart fired (in_speech == true at EOS time) AND
        // a DeviceError appears (the EOS flush tripped it) — without
        // both, the test isn't exercising the bug condition.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PipelineEvent::SpeechStart)),
            "test setup must trigger SpeechStart before VAD errors; got: {events:?}",
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PipelineEvent::DeviceError(_))),
            "EOS flush must trip a DeviceError; got: {events:?}",
        );
        // Contract: no SpeechEnd (or any other event) after the
        // DeviceError. The pre-fix code would have emitted SpeechEnd
        // here because `if vad_state.in_speech()` was checked without
        // also checking the `alive` flag.
        let err_idx = events
            .iter()
            .position(|e| matches!(e, PipelineEvent::DeviceError(_)))
            .expect("DeviceError present (asserted above)");
        // Only the FIRST event after the DeviceError matters for the
        // diagnostic — the loop variant above would only ever run one
        // iteration anyway. Pulling that element out via `.get(...)`
        // sidesteps the never-loop clippy lint and keeps the panic
        // message identical.
        if let Some(ev) = events.get(err_idx + 1) {
            let i = err_idx + 1;
            panic!("no events allowed after DeviceError, but events[{i}] = {ev:?} in: {events:?}",);
        }
    }
}
