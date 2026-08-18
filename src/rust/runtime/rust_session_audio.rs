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
//! * `audio-capture` -- provides [`RawCapturePipeline`].
//!
//! When the audio feature is missing the parent module surfaces a
//! human-readable error and the sink falls back to the PR 4 stub
//! session, so a partial-feature build still wires the coordinator
//! without panicking.

#![cfg(feature = "audio-capture")]

use std::sync::mpsc::Sender;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::audio::{PipelineEvent, PipelineReceiver, RawCapturePipeline};
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};
use crate::runtime::{RepaintNotifier, RuntimeEvent};

/// One-shot prefix every audio-pump status / error line carries so a
/// user grepping their log can pin the source.
const PUMP_LOG_PREFIX: &str = "[rust-session-audio]";
const DEVICE_RECOVERY_ATTEMPTS: usize = 3;
const DEVICE_RECOVERY_DELAY: Duration = Duration::from_secs(1);

/// Owns the running [`RawCapturePipeline`] + the pump thread that forwards
/// frames into the session. Dropping the pump tears down the pipeline
/// (which signals EOS on the cpal side; the pump thread sees the
/// channel close and exits naturally).
pub(crate) struct AudioPump {
    pipeline: Arc<Mutex<Option<RawCapturePipeline>>>,
    stop_requested: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for AudioPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPump")
            .field(
                "pipeline",
                &self
                    .pipeline
                    .lock()
                    .map(|pipeline| pipeline.as_ref().map(|_| "<RawCapturePipeline>"))
                    .unwrap_or(Some("<poisoned>")),
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
    /// hold it for minutes. Frames observed while the session is busy are
    /// discarded and summarized at debug/trace level; the bounded upstream
    /// queues additionally retain only their newest audio if this pump itself
    /// is descheduled.
    ///
    /// `tx` is the runtime event channel; the pump forwards a single
    /// `[rust-session-audio]` stderr line per [`PipelineEvent::DeviceError`].
    /// A device error retries the same configured device a bounded number of
    /// times, which lets WASAPI recover after a USB/Bluetooth profile change
    /// without silently retaining a dead capture stream. Optionally wakes the
    /// egui UI on every device-error event via the supplied `RepaintNotifier`.
    pub(crate) fn spawn_for_session_with_device<T, I>(
        session: Arc<Mutex<DictateSession<T, I>>>,
        tx: Sender<RuntimeEvent>,
        repaint_notifier: Option<RepaintNotifier>,
        device: &str,
    ) -> Result<Self, anyhow::Error>
    where
        T: TranscribeBackend + Send + 'static,
        I: InjectBackend + Send + 'static,
    {
        let (capture, rx) = RawCapturePipeline::start(&device)?;
        let pipeline = Arc::new(Mutex::new(Some(capture)));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let pipeline_for_pump = Arc::clone(&pipeline);
        let stop_for_pump = Arc::clone(&stop_requested);
        let device = device.to_owned();
        let pump = thread::Builder::new()
            .name("rust-session-audio".to_owned())
            .spawn(move || {
                pump_loop_with_recovery(
                    rx,
                    &device,
                    pipeline_for_pump,
                    stop_for_pump,
                    session,
                    tx,
                    repaint_notifier,
                )
            })?;
        Ok(Self {
            pipeline,
            stop_requested,
            pump: Some(pump),
        })
    }

    /// Return a cheap lifecycle callback that closes CPAL immediately without
    /// waiting for a synchronous transcription held by the coordinator.
    pub(crate) fn capture_stop(&self) -> super::supervisor::CaptureStop {
        let pipeline = Arc::clone(&self.pipeline);
        let stop_requested = Arc::clone(&self.stop_requested);
        Arc::new(move || {
            stop_requested.store(true, Ordering::Release);
            stop_capture_pipeline(&pipeline);
        })
    }
}

impl Drop for AudioPump {
    fn drop(&mut self) {
        // Stop the pipeline first so the cpal worker signals EOS;
        // the pump thread sees the receiver disconnect and returns,
        // then we join it. Order matters: joining the pump first
        // would deadlock if cpal is still feeding frames.
        self.stop_requested.store(true, Ordering::Release);
        stop_capture_pipeline(&self.pipeline);
        if let Some(handle) = self.pump.take() {
            let _ = handle.join();
        }
    }
}

fn stop_capture_pipeline(pipeline: &Mutex<Option<RawCapturePipeline>>) {
    let capture = pipeline
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    if let Some(mut capture) = capture {
        if crate::diag::debug_enabled() {
            crate::diag::log!("[runtime/debug] audio capture stop requested");
        }
        capture.stop();
        if crate::diag::trace_enabled() {
            crate::diag::log!("[runtime/trace] audio capture stream closed");
        }
    }
}

/// The pump thread body. Pulled out of `spawn_for_session` so the
/// closure stays small and the function is unit-testable through
/// [`pump_loop_with_recv`] below.
fn pump_loop_with_recovery<T, I>(
    rx: PipelineReceiver,
    device: &str,
    pipeline: Arc<Mutex<Option<RawCapturePipeline>>>,
    stop_requested: Arc<AtomicBool>,
    session: Arc<Mutex<DictateSession<T, I>>>,
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
{
    let mut receiver = Some(rx);
    let mut recovery_attempt = 0usize;
    loop {
        let rx = match receiver.take() {
            Some(rx) => rx,
            None => {
                if stop_requested.load(Ordering::Acquire) {
                    return;
                }
                match RawCapturePipeline::start(device) {
                    Ok((capture, rx)) => {
                        // Hold the same lock that `capture_stop` uses while
                        // checking + publishing the replacement. A stop that
                        // wins before this lock is acquired drops `capture`; a
                        // stop that arrives afterwards takes this published
                        // capture and stops it. Either way we never leave a
                        // reopened microphone running after teardown begins.
                        let mut slot = pipeline.lock().unwrap_or_else(|poison| poison.into_inner());
                        if stop_requested.load(Ordering::Acquire) {
                            drop(slot);
                            drop(capture);
                            return;
                        }
                        *slot = Some(capture);
                        drop(slot);
                        crate::diag::log!(
                        "{PUMP_LOG_PREFIX} device recovery succeeded attempt={recovery_attempt}"
                    );
                        rx
                    }
                    Err(error) => {
                        if !schedule_device_recovery(
                            &stop_requested,
                            &tx,
                            repaint_notifier.as_ref(),
                            &mut recovery_attempt,
                            format!("reopen device: {error}"),
                        ) {
                            return;
                        }
                        continue;
                    }
                }
            }
        };
        let mut received_frame = false;
        let device_error = pump_loop_with_recv(
            || rx.recv().ok(),
            |frame| {
                // A callback frame proves the replacement stream is healthy;
                // reset the bounded retry budget so unrelated future device
                // losses get their own recovery window.
                received_frame = true;
                match session.try_lock() {
                    Ok(mut guard) => {
                        guard.push_frame(frame);
                        true
                    }
                    Err(TryLockError::Poisoned(poison)) => {
                        poison.into_inner().push_frame(frame);
                        true
                    }
                    Err(TryLockError::WouldBlock) => false,
                }
            },
            |dropped| {
                if crate::diag::debug_enabled() {
                    crate::diag::log!(
                    "{PUMP_LOG_PREFIX} discarded {dropped} frame(s) captured while the session was busy"
                    );
                }
            },
        );
        discard_capture_pipeline(&pipeline);
        reset_recovery_attempt_after_frame(&mut recovery_attempt, received_frame);
        let Some(error) = device_error else {
            return;
        };
        if !schedule_device_recovery(
            &stop_requested,
            &tx,
            repaint_notifier.as_ref(),
            &mut recovery_attempt,
            error,
        ) {
            return;
        }
    }
}

fn reset_recovery_attempt_after_frame(recovery_attempt: &mut usize, received_frame: bool) {
    if received_frame {
        *recovery_attempt = 0;
    }
}

fn discard_capture_pipeline(pipeline: &Mutex<Option<RawCapturePipeline>>) {
    let capture = pipeline
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    drop(capture);
}

fn schedule_device_recovery(
    stop_requested: &AtomicBool,
    tx: &Sender<RuntimeEvent>,
    repaint_notifier: Option<&RepaintNotifier>,
    recovery_attempt: &mut usize,
    error: String,
) -> bool {
    if stop_requested.load(Ordering::Acquire) {
        return false;
    }
    if *recovery_attempt >= DEVICE_RECOVERY_ATTEMPTS {
        let message = format!(
            "{PUMP_LOG_PREFIX} device recovery exhausted after {DEVICE_RECOVERY_ATTEMPTS} attempt(s): {error}"
        );
        let _ = tx.send(RuntimeEvent::Stderr(message));
        let _ = tx.send(RuntimeEvent::Exited { code: Some(1) });
        if let Some(notifier) = repaint_notifier {
            notifier();
        }
        return false;
    }
    *recovery_attempt += 1;
    let message = format!(
        "{PUMP_LOG_PREFIX} device error: {error}; reopening configured input (attempt {recovery_attempt}/{DEVICE_RECOVERY_ATTEMPTS})"
    );
    let _ = tx.send(RuntimeEvent::Stderr(message));
    if let Some(notifier) = repaint_notifier {
        notifier();
    }
    for _ in 0..20 {
        if stop_requested.load(Ordering::Acquire) {
            return false;
        }
        thread::sleep(DEVICE_RECOVERY_DELAY / 20);
    }
    true
}

/// Pure-logic pump loop with the channel + session sinks
/// supplied as closures so the unit tests can drive it without a real
/// `RawCapturePipeline` or `DictateSession`. The contract:
///
/// * `recv_next` returns the next [`PipelineEvent`] or `None` when the
///   channel has disconnected.
/// * `try_push_frame` is called for every [`PipelineEvent::Frame`]. It returns
///   false when the session is busy; the loop keeps draining instead of
///   allowing stale frames to queue behind transcription.
/// * `report_dropped` receives each completed consecutive drop count.
/// * A [`PipelineEvent::DeviceError`] is returned to the owner, which emits
///   the single diagnostic containing both its details and the recovery
///   decision.
///
fn pump_loop_with_recv<R, P, D>(
    mut recv_next: R,
    mut try_push_frame: P,
    mut report_dropped: D,
) -> Option<String>
where
    R: FnMut() -> Option<PipelineEvent>,
    P: FnMut(&[f32]) -> bool,
    D: FnMut(usize),
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
            PipelineEvent::DeviceError(msg) => {
                if dropped > 0 {
                    report_dropped(dropped);
                }
                // Per the `PipelineEvent::DeviceError` wire contract
                // ("no further messages after device_error") the pump
                // thread MUST stop here. The owner can then either reopen a
                // fresh pipeline or terminate the runtime.
                return Some(msg);
            }
        }
    }
    if dropped > 0 {
        report_dropped(dropped);
    }
    None
}

#[cfg(test)]
#[path = "rust_session_audio_tests.rs"]
mod tests;
