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

use std::cell::Cell;
use std::sync::mpsc::Sender;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex, RwLock, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::audio::{PipelineEvent, PipelineReceiver, RawCapturePipeline};
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};
use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

/// One-shot prefix every audio-pump status / error line carries so a
/// user grepping their log can pin the source.
const PUMP_LOG_PREFIX: &str = "[rust-session-audio]";
const DEVICE_RECOVERY_ATTEMPTS: usize = 3;
const DEVICE_RECOVERY_DELAY: Duration = Duration::from_secs(1);
const RECOVERY_HEALTHY_FRAME_COUNT: usize = 50;
const RECOVERY_VALIDATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Which input an in-process capture recovery should open next.
///
/// A configured selector remains the user's preference. The system default is
/// an in-memory fallback only, so a temporary USB/Bluetooth disconnect never
/// silently rewrites the Settings choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryTarget {
    Configured,
    SystemDefault,
}

impl RecoveryTarget {
    fn selector<'a>(self, configured_device: &'a str) -> &'a str {
        match self {
            Self::Configured => configured_device,
            // An empty selector is the CPAL default-host default-input device
            // on Windows, Linux and macOS.
            Self::SystemDefault => "",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Configured => "configured input",
            Self::SystemDefault => "system default input",
        }
    }
}

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
    /// A device error retries the configured device a bounded number of times,
    /// then keeps the runtime alive on the operating system's default input.
    /// This lets WASAPI and other hosts recover after a USB/Bluetooth profile
    /// change without silently retaining a dead capture stream. Optionally
    /// wakes the egui UI on every device-error event via the supplied
    /// `RepaintNotifier`.
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
        let effective_audio_device = session
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .effective_audio_device_handle();
        let (capture, rx, configured_error) = start_initial_capture(&device)?;
        if let Some(configured_error) = configured_error {
            set_effective_audio_device(&effective_audio_device, "System default");
            let message = format!(
                "{PUMP_LOG_PREFIX} configured input unavailable at startup ({configured_error}); using system default input"
            );
            let _ = tx.send(RuntimeEvent::Stderr(message));
            send_audio_status(&tx, "audio-fallback", Some("System default"), None, None);
        }
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
                    effective_audio_device,
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
    effective_audio_device: Arc<RwLock<String>>,
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
{
    let mut receiver = Some(rx);
    let mut recovery_attempt = 0usize;
    let mut configured_timeout_circuit_open = false;
    let mut next_target = RecoveryTarget::Configured;
    loop {
        let validating_target = Cell::new(None);
        let rx = match receiver.take() {
            Some(rx) => rx,
            None => {
                if stop_requested.load(Ordering::Acquire) {
                    return;
                }
                match open_recovery_target(next_target, device, RawCapturePipeline::start) {
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
                        validating_target.set(Some(next_target));
                        crate::diag::log!(
                            "{PUMP_LOG_PREFIX} device recovery candidate opened target={} attempt={recovery_attempt}; validating frames",
                            next_target.description()
                        );
                        rx
                    }
                    Err(error) => {
                        let retry_open = record_recovery_open_timeout(
                            next_target,
                            crate::audio::capture::is_capture_start_timeout(&error),
                            &mut recovery_attempt,
                            &mut configured_timeout_circuit_open,
                        );
                        if !retry_open {
                            mark_capture_unavailable(&effective_audio_device);
                        }
                        if !report_recovery_open_failure(
                            &stop_requested,
                            &tx,
                            repaint_notifier.as_ref(),
                            next_target,
                            &error.to_string(),
                            retry_open,
                        ) {
                            return;
                        }
                        if !retry_open {
                            return;
                        }
                        let Some(target) = schedule_device_recovery(
                            &stop_requested,
                            &tx,
                            repaint_notifier.as_ref(),
                            &mut recovery_attempt,
                            device,
                            format!("reopen device: {error}"),
                        ) else {
                            return;
                        };
                        next_target = target;
                        continue;
                    }
                }
            }
        };
        let mut received_frames = 0usize;
        let device_error = pump_loop_with_recv(
            || {
                recv_pipeline_event(
                    &rx,
                    validating_target.get().is_some(),
                    RECOVERY_VALIDATION_TIMEOUT,
                )
            },
            |frame| {
                // Do not let a flapping USB/Bluetooth stream reset the retry
                // budget after one callback. Roughly 1.5 seconds of 30 ms
                // frames proves the replacement was genuinely healthy.
                received_frames = received_frames.saturating_add(1);
                let recovered_target = apply_validated_recovery(
                    &stop_requested,
                    &effective_audio_device,
                    &validating_target,
                    received_frames,
                    device,
                );
                let accepted = match session.try_lock() {
                    Ok(mut guard) => {
                        guard.push_frame(frame);
                        true
                    }
                    Err(TryLockError::Poisoned(poison)) => {
                        let mut guard = poison.into_inner();
                        guard.push_frame(frame);
                        true
                    }
                    Err(TryLockError::WouldBlock) => false,
                };
                if let Some(target) = recovered_target {
                    if !stop_requested.load(Ordering::Acquire) {
                        publish_recovery_status(&tx, repaint_notifier.as_ref(), target, device);
                    }
                }
                accepted
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
        reset_recovery_attempt_after_frame(
            &mut recovery_attempt,
            received_frames,
            configured_timeout_circuit_open,
        );
        let Some(error) = device_error else {
            return;
        };
        let Some(target) = schedule_device_recovery(
            &stop_requested,
            &tx,
            repaint_notifier.as_ref(),
            &mut recovery_attempt,
            device,
            error,
        ) else {
            return;
        };
        next_target = target;
    }
}

fn reset_recovery_attempt_after_frame(
    recovery_attempt: &mut usize,
    received_frames: usize,
    configured_timeout_circuit_open: bool,
) {
    if received_frames >= RECOVERY_HEALTHY_FRAME_COUNT && !configured_timeout_circuit_open {
        *recovery_attempt = 0;
    }
}

fn apply_validated_recovery(
    stop_requested: &AtomicBool,
    effective_audio_device: &RwLock<String>,
    validating_target: &Cell<Option<RecoveryTarget>>,
    received_frames: usize,
    configured_device: &str,
) -> Option<RecoveryTarget> {
    if stop_requested.load(Ordering::Acquire) {
        return None;
    }
    let target = take_validated_recovery_target(validating_target, received_frames)?;
    set_effective_audio_device(
        effective_audio_device,
        effective_device_for_target(target, configured_device),
    );
    Some(target)
}

fn set_effective_audio_device(effective_audio_device: &RwLock<String>, device: &str) {
    *effective_audio_device
        .write()
        .unwrap_or_else(|poison| poison.into_inner()) = device.to_owned();
}

fn mark_capture_unavailable(effective_audio_device: &RwLock<String>) {
    set_effective_audio_device(effective_audio_device, "");
}

fn take_validated_recovery_target(
    validating_target: &Cell<Option<RecoveryTarget>>,
    received_frames: usize,
) -> Option<RecoveryTarget> {
    (received_frames >= RECOVERY_HEALTHY_FRAME_COUNT)
        .then(|| validating_target.take())
        .flatten()
}

fn recv_pipeline_event(
    rx: &PipelineReceiver,
    validating_recovery: bool,
    validation_timeout: Duration,
) -> Option<PipelineEvent> {
    if !validating_recovery {
        return rx.recv().ok();
    }
    match rx.recv_timeout(validation_timeout) {
        Ok(event) => Some(event),
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => Some(PipelineEvent::DeviceError(
            format!(
                "recovery candidate did not produce {RECOVERY_HEALTHY_FRAME_COUNT} healthy frames within {} seconds",
                validation_timeout.as_secs()
            ),
        )),
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => None,
    }
}

fn discard_capture_pipeline(pipeline: &Mutex<Option<RawCapturePipeline>>) {
    let capture = pipeline
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    drop(capture);
}

/// Open the saved microphone first. If it is unavailable at startup, use the
/// OS default input for this runtime instance without mutating the saved
/// selector. Once capture was running, [`pump_loop_with_recovery`] applies the
/// same fallback after its bounded configured-device retry window.
fn start_initial_capture(
    configured_device: &str,
) -> Result<(RawCapturePipeline, PipelineReceiver, Option<String>), anyhow::Error> {
    start_initial_capture_with(configured_device, RawCapturePipeline::start)
        .map(|((capture, rx), configured_error)| (capture, rx, configured_error))
}

fn start_initial_capture_with<T>(
    configured_device: &str,
    mut open: impl FnMut(&str) -> Result<T, anyhow::Error>,
) -> Result<(T, Option<String>), anyhow::Error> {
    match open(configured_device) {
        Ok(capture) => Ok((capture, None)),
        Err(configured_error) if should_try_system_default(configured_device) => {
            let configured_error = configured_error.to_string();
            open("")
                .map(|capture| (capture, Some(configured_error.clone())))
                .map_err(|default_error| {
                    anyhow::anyhow!(
                        "could not open configured input ({configured_error}); system default input also failed ({default_error})"
                    )
                })
        }
        Err(error) => Err(error),
    }
}

/// A named saved device can be unavailable while the OS default remains
/// captureable. An empty selector already *is* the OS default, so retrying it
/// here would add no recovery path.
fn should_try_system_default(configured_device: &str) -> bool {
    !configured_device.trim().is_empty()
}

fn open_recovery_target<T, E>(
    target: RecoveryTarget,
    configured_device: &str,
    open: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T, E> {
    open(target.selector(configured_device))
}

fn send_audio_status(
    tx: &Sender<RuntimeEvent>,
    state: &str,
    audio_device: Option<&str>,
    reason: Option<&str>,
    error: Option<&str>,
) {
    let mut payload = serde_json::Map::new();
    payload.insert("event".to_owned(), serde_json::Value::from("status"));
    payload.insert("state".to_owned(), serde_json::Value::from(state));
    if let Some(device) = audio_device {
        payload.insert("audio_device".to_owned(), serde_json::Value::from(device));
    }
    if let Some(reason) = reason {
        payload.insert("reason".to_owned(), serde_json::Value::from(reason));
    }
    if let Some(error) = error {
        payload.insert("error".to_owned(), serde_json::Value::from(error));
    }
    let _ = tx.send(RuntimeEvent::Worker(WorkerEvent {
        event: "status".to_owned(),
        state: Some(state.to_owned()),
        payload: serde_json::Value::Object(payload),
    }));
}

fn report_recovery_open_failure(
    stop_requested: &AtomicBool,
    tx: &Sender<RuntimeEvent>,
    repaint_notifier: Option<&RepaintNotifier>,
    target: RecoveryTarget,
    error: &str,
    retry_open: bool,
) -> bool {
    if stop_requested.load(Ordering::Acquire) {
        return false;
    }
    if target == RecoveryTarget::SystemDefault {
        send_audio_status(
            tx,
            "error",
            None,
            Some("device_unusable"),
            Some(&if retry_open {
                format!(
                    "System default microphone is unavailable; retrying in the background: {error}"
                )
            } else {
                format!(
                    "System default microphone did not finish opening; background recovery is paused to avoid accumulating blocked audio-driver threads. Restart the dictation runtime to retry: {error}"
                )
            }),
        );
        if let Some(notifier) = repaint_notifier {
            notifier();
        }
    }
    true
}

fn record_recovery_open_timeout(
    target: RecoveryTarget,
    timed_out: bool,
    recovery_attempt: &mut usize,
    configured_timeout_circuit_open: &mut bool,
) -> bool {
    if !timed_out {
        return true;
    }
    match target {
        RecoveryTarget::Configured => {
            *configured_timeout_circuit_open = true;
            *recovery_attempt = DEVICE_RECOVERY_ATTEMPTS;
            true
        }
        RecoveryTarget::SystemDefault => false,
    }
}

fn publish_recovery_status(
    tx: &Sender<RuntimeEvent>,
    repaint_notifier: Option<&RepaintNotifier>,
    target: RecoveryTarget,
    configured_device: &str,
) {
    let active_device = effective_device_for_target(target, configured_device);
    send_audio_status(tx, "audio-recovered", Some(active_device), None, None);
    if let Some(notifier) = repaint_notifier {
        notifier();
    }
}

fn effective_device_for_target(target: RecoveryTarget, configured_device: &str) -> &str {
    match target {
        RecoveryTarget::Configured => configured_device,
        RecoveryTarget::SystemDefault => "System default",
    }
}

fn next_recovery_target(configured_device: &str, recovery_attempt: &mut usize) -> RecoveryTarget {
    if configured_device.trim().is_empty() || *recovery_attempt >= DEVICE_RECOVERY_ATTEMPTS {
        RecoveryTarget::SystemDefault
    } else {
        *recovery_attempt += 1;
        RecoveryTarget::Configured
    }
}

fn schedule_device_recovery(
    stop_requested: &AtomicBool,
    tx: &Sender<RuntimeEvent>,
    repaint_notifier: Option<&RepaintNotifier>,
    recovery_attempt: &mut usize,
    configured_device: &str,
    error: String,
) -> Option<RecoveryTarget> {
    if stop_requested.load(Ordering::Acquire) {
        return None;
    }
    let target = next_recovery_target(configured_device, recovery_attempt);
    let message = format!(
        "{PUMP_LOG_PREFIX} device error: {error}; reopening {}{}",
        target.description(),
        match target {
            RecoveryTarget::Configured => {
                format!(" (attempt {recovery_attempt}/{DEVICE_RECOVERY_ATTEMPTS})")
            }
            RecoveryTarget::SystemDefault if configured_device.trim().is_empty() => {
                " (runtime stays active)".to_owned()
            }
            RecoveryTarget::SystemDefault => {
                " (configured retries exhausted; runtime stays active)".to_owned()
            }
        }
    );
    let _ = tx.send(RuntimeEvent::Stderr(message));
    if let Some(notifier) = repaint_notifier {
        notifier();
    }
    for _ in 0..20 {
        if stop_requested.load(Ordering::Acquire) {
            return None;
        }
        thread::sleep(DEVICE_RECOVERY_DELAY / 20);
    }
    Some(target)
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
