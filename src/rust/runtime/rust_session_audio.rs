//! Wave 5 PR 5 of #348 finding 1 (Codex P1 #423
//! rust_session_sink.rs:295): wire the Rust audio pipeline into the
//! real [`crate::dictate::DictateSession`] so captured frames actually
//! reach the transcriber.
//!
//! Before this module the rust-session real-backend sink installed
//! [`crate::dictate::backends::WhisperLocalTranscribeBackend`] +
//! [`crate::dictate::backends::EnigoInjectBackend`] but no production
//! caller ever fed [`crate::dictate::DictateSession::push_frame`] any
//! audio, so every PTT release hit the `no_audio` early-return inside
//! `stop_and_transcribe` and the real transcriber was never invoked.
//!
//! This module spins up an [`crate::audio::AudioPipeline`] (cpal ->
//! resampler -> Silero VAD) the moment the real-backend sink is built
//! and forwards every [`PipelineEvent::Frame`] into
//! [`crate::dictate::DictateSession::push_frame`] on a background pump
//! thread. The session itself drops idle frames when not in
//! [`crate::dictate::SessionState::Recording`], so the pump runs
//! continuously between PTT presses without polluting the buffer.
//!
//! # Gating
//!
//! Compiled in only when ALL THREE features are on:
//!
//! * `whisper-rs-local` -- the parent
//!   [`super::rust_session_real_backends`] module is gated on this.
//! * `rust-injection` -- same.
//! * `audio-in-rust` -- this module's own gate; without it cpal /
//!   Silero / the [`crate::audio::AudioPipeline`] type do not exist.
//!
//! When the audio feature is missing the parent module surfaces a
//! human-readable error and the sink falls back to the PR 4 stub
//! session, so a partial-feature build still wires the coordinator
//! without panicking.

#![cfg(feature = "audio-in-rust")]

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::audio::{default_silero_loader, AudioPipeline, PipelineEvent};
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};
use crate::runtime::audio_spawn::resolve_audio_device_from_env;
use crate::runtime::{RepaintNotifier, RuntimeEvent};

/// One-shot prefix every audio-pump status / error line carries so a
/// user grepping their log can pin the source.
const PUMP_LOG_PREFIX: &str = "[rust-session-audio]";

/// Owns the running [`AudioPipeline`] + the pump thread that forwards
/// frames into the session. Dropping the pump tears down the pipeline
/// (which signals EOS on the cpal side; the pump thread sees the
/// channel close and exits naturally).
pub(crate) struct AudioPump {
    pipeline: Option<AudioPipeline>,
    pump: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for AudioPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPump")
            .field(
                "pipeline",
                &self.pipeline.as_ref().map(|_| "<AudioPipeline>"),
            )
            .field("pump", &self.pump.as_ref().map(|_| "<JoinHandle>"))
            .finish()
    }
}

impl AudioPump {
    /// Open the cpal capture stream and spawn the forwarder thread.
    ///
    /// `session` is the same `Arc<Mutex<...>>` the coordinator-sink
    /// closure holds; the pump locks it briefly per frame to call
    /// [`DictateSession::push_frame`]. Lock contention is bounded --
    /// the sink only holds the lock during a coordinator action
    /// (start / stop / cancel) and frames arrive at 16 kHz / 480
    /// samples (= one every 30 ms), so the worst case is the pump
    /// parking for ~one transcribe latency before the next frame
    /// lands. The session drops the frame backlog when it returns to
    /// Idle, so the brief stall does not bloat the buffer.
    ///
    /// `tx` is the runtime event channel; the pump forwards a single
    /// `[rust-session-audio]` stderr line per [`PipelineEvent::DeviceError`]
    /// and exits. Optionally wakes the egui UI on every device-error
    /// event via the supplied `RepaintNotifier`.
    pub(crate) fn spawn_for_session<T, I>(
        session: Arc<Mutex<DictateSession<T, I>>>,
        tx: Sender<RuntimeEvent>,
        repaint_notifier: Option<RepaintNotifier>,
    ) -> Result<Self, anyhow::Error>
    where
        T: TranscribeBackend + Send + 'static,
        I: InjectBackend + Send + 'static,
    {
        // Resolve the configured microphone the same way the
        // existing Python-backend audio bridge does. Empty string =
        // OS default; `audio::capture::start_capture` honours that.
        let device = resolve_audio_device_from_env(&[]);
        let (pipeline, rx) = AudioPipeline::start(&device, default_silero_loader())?;
        let pump = thread::Builder::new()
            .name("rust-session-audio".to_owned())
            .spawn(move || pump_loop(rx, session, tx, repaint_notifier))?;
        Ok(Self {
            pipeline: Some(pipeline),
            pump: Some(pump),
        })
    }
}

impl Drop for AudioPump {
    fn drop(&mut self) {
        // Stop the pipeline first so the cpal worker signals EOS;
        // the pump thread sees the receiver disconnect and returns,
        // then we join it. Order matters: joining the pump first
        // would deadlock if cpal is still feeding frames.
        if let Some(mut p) = self.pipeline.take() {
            p.stop();
        }
        if let Some(handle) = self.pump.take() {
            let _ = handle.join();
        }
    }
}

/// The pump thread body. Pulled out of `spawn_for_session` so the
/// closure stays small and the function is unit-testable through
/// [`pump_loop_with_recv`] below.
fn pump_loop<T, I>(
    rx: std::sync::mpsc::Receiver<PipelineEvent>,
    session: Arc<Mutex<DictateSession<T, I>>>,
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
{
    pump_loop_with_recv(
        || rx.recv().ok(),
        |frame| {
            let mut guard = session.lock().unwrap_or_else(|p| p.into_inner());
            guard.push_frame(frame);
        },
        |line| {
            let _ = tx.send(RuntimeEvent::Stderr(line));
            if let Some(notifier) = repaint_notifier.as_ref() {
                notifier();
            }
        },
    );
}

/// Pure-logic pump loop with the channel + session + log sinks
/// supplied as closures so the unit tests can drive it without a real
/// `AudioPipeline` or `DictateSession`. The contract:
///
/// * `recv_next` returns the next [`PipelineEvent`] or `None` when the
///   channel has disconnected.
/// * `push_frame` is called for every [`PipelineEvent::Frame`] before
///   the loop continues.
/// * `log_line` is called once per [`PipelineEvent::DeviceError`] with
///   a `[rust-session-audio] ...` prefix, after which the loop exits
///   (the device error is terminal per the wire contract documented
///   on [`PipelineEvent::DeviceError`]).
///
/// `SpeechStart`, `SpeechEnd`, and `Cancelled` are dropped silently --
/// they carry no payload the session can consume directly, and the
/// PTT-release boundary owns utterance commits. Mirrors the
/// `vp_capture_rust_stdin.py` ignore list for those event variants.
fn pump_loop_with_recv<R, P, L>(mut recv_next: R, mut push_frame: P, mut log_line: L)
where
    R: FnMut() -> Option<PipelineEvent>,
    P: FnMut(&[f32]),
    L: FnMut(String),
{
    while let Some(event) = recv_next() {
        match event {
            PipelineEvent::Frame(frame) => push_frame(&frame),
            PipelineEvent::SpeechStart | PipelineEvent::SpeechEnd | PipelineEvent::Cancelled => {
                // No-op: the session does not consume VAD markers
                // (the PTT coordinator owns recording lifecycle); the
                // Cancelled passthrough is deliberately Python-Phase-1
                // compatible -- see `vp_capture_rust_stdin.py:228-232`.
            }
            PipelineEvent::DeviceError(msg) => {
                log_line(format!("{PUMP_LOG_PREFIX} device error: {msg}"));
                // Per the `PipelineEvent::DeviceError` wire contract
                // ("no further messages after device_error") the pump
                // thread MUST stop here. The supervisor can re-spawn
                // a fresh pump on the next process restart; live
                // recovery is a Wave-6 follow-up.
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "rust_session_audio_tests.rs"]
mod tests;
