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
//! This module spins up a VAD-free [`crate::audio::RawCapturePipeline`]
//! (cpal -> resampler) the moment the real-backend sink is built and forwards
//! every [`PipelineEvent::Frame`] into
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
//! * `audio-in-rust` -- this module's existing parent gate; it implies the
//!   lighter `audio-capture` feature that provides RawCapturePipeline.
//!
//! When the audio feature is missing the parent module surfaces a
//! human-readable error and the sink falls back to the PR 4 stub
//! session, so a partial-feature build still wires the coordinator
//! without panicking.

#![cfg(feature = "audio-in-rust")]

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};

use crate::audio::{PipelineEvent, RawCapturePipeline};
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};
use crate::runtime::audio_spawn::resolve_audio_device_from_env;
use crate::runtime::{RepaintNotifier, RuntimeEvent};

/// One-shot prefix every audio-pump status / error line carries so a
/// user grepping their log can pin the source.
const PUMP_LOG_PREFIX: &str = "[rust-session-audio]";

/// Owns the running [`RawCapturePipeline`] + the pump thread that forwards
/// frames into the session. Dropping the pump tears down the pipeline
/// (which signals EOS on the cpal side; the pump thread sees the
/// channel close and exits naturally).
pub(crate) struct AudioPump {
    pipeline: Option<RawCapturePipeline>,
    pump: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for AudioPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPump")
            .field(
                "pipeline",
                &self.pipeline.as_ref().map(|_| "<RawCapturePipeline>"),
            )
            .field("pump", &self.pump.as_ref().map(|_| "<JoinHandle>"))
            .finish()
    }
}

impl AudioPump {
    /// Open the cpal capture stream and spawn the forwarder thread.
    ///
    /// `session` is the same `Arc<Mutex<...>>` the coordinator-sink
    /// closure holds. The pump never waits for that mutex: transcription can
    /// hold it for minutes, and blocking here would let post-release audio
    /// accumulate in cpal's unbounded event channel and leak into a pending
    /// next recording. Frames observed while the session is busy are discarded
    /// and summarized at debug/trace level.
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
        let (pipeline, rx) = RawCapturePipeline::start(&device)?;
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
        |frame| match session.try_lock() {
            Ok(mut guard) => {
                guard.push_frame(frame);
                true
            }
            Err(TryLockError::Poisoned(poison)) => {
                poison.into_inner().push_frame(frame);
                true
            }
            Err(TryLockError::WouldBlock) => false,
        },
        |dropped| {
            if crate::diag::debug_enabled() {
                crate::diag::log!(
                    "{PUMP_LOG_PREFIX} discarded {dropped} frame(s) captured while the session was busy"
                );
            }
        },
        |line| {
            let _ = tx.send(RuntimeEvent::Stderr(line));
            let _ = tx.send(RuntimeEvent::Exited { code: Some(1) });
            if let Some(notifier) = repaint_notifier.as_ref() {
                notifier();
            }
        },
    );
}

/// Pure-logic pump loop with the channel + session + log sinks
/// supplied as closures so the unit tests can drive it without a real
/// `RawCapturePipeline` or `DictateSession`. The contract:
///
/// * `recv_next` returns the next [`PipelineEvent`] or `None` when the
///   channel has disconnected.
/// * `try_push_frame` is called for every [`PipelineEvent::Frame`]. It returns
///   false when the session is busy; the loop keeps draining instead of
///   allowing stale frames to queue behind transcription.
/// * `report_dropped` receives each completed consecutive drop count.
/// * `log_line` is called once per [`PipelineEvent::DeviceError`] with
///   a `[rust-session-audio] ...` prefix, after which the loop exits
///   (the device error is terminal per the wire contract documented
///   on [`PipelineEvent::DeviceError`]).
///
/// `SpeechStart`, `SpeechEnd`, and `Cancelled` are dropped silently --
/// they carry no payload the session can consume directly, and the
/// PTT-release boundary owns utterance commits. Mirrors the
/// `vp_capture_rust_stdin.py` ignore list for those event variants.
fn pump_loop_with_recv<R, P, D, L>(
    mut recv_next: R,
    mut try_push_frame: P,
    mut report_dropped: D,
    mut log_line: L,
) where
    R: FnMut() -> Option<PipelineEvent>,
    P: FnMut(&[f32]) -> bool,
    D: FnMut(usize),
    L: FnMut(String),
{
    let mut dropped = 0usize;
    while let Some(event) = recv_next() {
        match event {
            PipelineEvent::Frame(frame) => {
                if try_push_frame(&frame) {
                    if dropped > 0 {
                        report_dropped(dropped);
                        dropped = 0;
                    }
                } else {
                    dropped = dropped.saturating_add(1);
                    if dropped == 1 && crate::diag::trace_enabled() {
                        crate::diag::log!(
                            "{PUMP_LOG_PREFIX} session busy; draining and discarding captured frames"
                        );
                    }
                }
            }
            PipelineEvent::SpeechStart | PipelineEvent::SpeechEnd | PipelineEvent::Cancelled => {
                // No-op: the session does not consume VAD markers
                // (the PTT coordinator owns recording lifecycle); the
                // Cancelled passthrough is deliberately Python-Phase-1
                // compatible -- see `vp_capture_rust_stdin.py:228-232`.
            }
            PipelineEvent::DeviceError(msg) => {
                if dropped > 0 {
                    report_dropped(dropped);
                }
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
    if dropped > 0 {
        report_dropped(dropped);
    }
}

#[cfg(test)]
#[path = "rust_session_audio_tests.rs"]
mod tests;
