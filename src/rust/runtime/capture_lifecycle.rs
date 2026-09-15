//! Push-to-talk microphone lifecycle (#323): the capture device is open only
//! while recording.
//!
//! * Runtime start validates the input by enumeration; no stream is opened.
//! * [`RecordingCapture::open_for_recording`] opens the configured input (or
//!   the OS default for this recording when it fails) and spawns a forwarder
//!   thread that feeds every delivered frame into the session.
//! * [`RecordingCapture::close_for_recording`] closes the stream and joins the
//!   forwarder so the release tail reaches the session before transcription.
//! * [`CaptureLifecycle::capture_stop`] and `Drop` close any open stream and
//!   refuse later opens.
//!
//! A device error while recording is reported once and the device stays
//! closed until the next press; nothing reopens a stream while idle.

#![cfg(feature = "audio-capture")]
#![cfg_attr(
    not(all(feature = "whisper-rs-local", feature = "rust-injection")),
    allow(dead_code)
)]

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::capture_forwarder::{forward_frames, ForwardEnd, FrameSink};
use super::capture_open_policy::{
    has_named_device, open_with_fallback, probe_startup_device, OpenBreaker, OpenTarget,
    StartupDevice,
};
use super::capture_status::{CaptureReporter, LOG_PREFIX, SYSTEM_DEFAULT_LABEL};
use super::recording_capture::RecordingCapture;
use super::supervisor::CaptureStop;
use crate::audio::PipelineReceiver;

/// Opens slower than this are surfaced on the runtime channel because the
/// start of speech may have been missed (Bluetooth profile switches can
/// take around a second).
pub(crate) const SLOW_OPEN_WARNING: Duration = Duration::from_millis(300);

const HEALTH_OK: u8 = 0;
const HEALTH_FALLBACK: u8 = 1;
const HEALTH_UNAVAILABLE: u8 = 2;
const HEALTH_STARTUP_FALLBACK: u8 = 3;

/// Opens capture streams. Production wraps `RawCapturePipeline`; tests use a
/// fake so the lifecycle is verifiable without audio hardware.
pub(crate) trait CaptureOpener: Send + Sync + 'static {
    /// Dropping the stream closes the device and disconnects its receiver.
    type Stream: Send + 'static;

    /// Check that `selector` names an existing input without opening it.
    fn probe(&self, selector: &str) -> Result<(), anyhow::Error>;

    fn open(&self, selector: &str) -> Result<(Self::Stream, PipelineReceiver), anyhow::Error>;

    fn is_timeout(&self, error: &anyhow::Error) -> bool;
}

struct Slot<S> {
    stream: Option<S>,
    stopped: bool,
}

pub(crate) struct CaptureLifecycle<O: CaptureOpener, F: FrameSink> {
    opener: O,
    frames: Arc<F>,
    configured: String,
    slot: Arc<Mutex<Slot<O::Stream>>>,
    /// Also serializes open/close; never taken by `capture_stop`.
    forwarder: Mutex<Option<JoinHandle<()>>>,
    breaker: Mutex<OpenBreaker>,
    health: Arc<AtomicU8>,
    reporter: Arc<CaptureReporter>,
    slow_open_warning: Duration,
}

impl<O: CaptureOpener, F: FrameSink> CaptureLifecycle<O, F> {
    /// Validate the configured input by enumeration. No stream is opened.
    pub(crate) fn new(
        opener: O,
        frames: Arc<F>,
        configured: &str,
        reporter: CaptureReporter,
    ) -> Result<Self, anyhow::Error> {
        let startup = probe_startup_device(configured, |selector| opener.probe(selector))?;
        let health = match &startup {
            StartupDevice::Configured | StartupDevice::SystemDefault => HEALTH_OK,
            StartupDevice::ConfiguredMissing { error } => {
                reporter.set_effective_device(SYSTEM_DEFAULT_LABEL);
                reporter.stderr(format!(
                    "{LOG_PREFIX} configured input not found at startup ({error}); the system default input will be used when recording starts"
                ));
                HEALTH_STARTUP_FALLBACK
            }
        };
        crate::diag::log!(
            "{LOG_PREFIX} input validated without opening a stream ({startup:?}); microphone stays closed until push-to-talk"
        );
        Ok(Self {
            opener,
            frames,
            configured: configured.to_owned(),
            slot: Arc::new(Mutex::new(Slot {
                stream: None,
                stopped: false,
            })),
            forwarder: Mutex::new(None),
            breaker: Mutex::new(OpenBreaker::default()),
            health: Arc::new(AtomicU8::new(health)),
            reporter: Arc::new(reporter),
            slow_open_warning: SLOW_OPEN_WARNING,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_slow_open_warning(mut self, threshold: Duration) -> Self {
        self.slow_open_warning = threshold;
        self
    }

    /// Close callback for runtime teardown. Never waits for a recording,
    /// an in-flight open, or transcription.
    pub(crate) fn capture_stop(&self) -> CaptureStop {
        let slot = Arc::clone(&self.slot);
        Arc::new(move || stop_slot(&slot))
    }

    fn open(&self) {
        let mut forwarder = lock(&self.forwarder);
        if forwarder.is_some() {
            crate::diag::log!("{LOG_PREFIX} capture already open for this recording");
            return;
        }
        if lock(&self.slot).stopped {
            return;
        }
        let started = Instant::now();
        let result = {
            let mut breaker = lock(&self.breaker);
            open_with_fallback(
                &self.configured,
                &mut breaker,
                |selector| self.opener.open(selector),
                |error| self.opener.is_timeout(error),
            )
        };
        let opened = match result {
            Ok(opened) => opened,
            Err(failure) => return self.report_open_failure(&failure.message, failure.paused),
        };
        let elapsed = started.elapsed();
        let (stream, rx) = opened.stream;
        if !install_stream(&self.slot, stream) {
            crate::diag::log!("{LOG_PREFIX} capture opened after runtime stop; closed immediately");
            return;
        }
        crate::diag::log!(
            "{LOG_PREFIX} capture opened in {} ms (target={})",
            elapsed.as_millis(),
            opened.target.description()
        );
        self.report_opened(opened.target, opened.configured_error.as_deref(), elapsed);
        match self.spawn_forwarder(rx) {
            Ok(handle) => *forwarder = Some(handle),
            Err(error) => {
                drop(take_stream(&self.slot));
                self.reporter.capture_unavailable(&format!(
                    "Microphone capture could not start its forwarding thread: {error}"
                ));
            }
        }
    }

    fn close(&self) {
        let mut forwarder = lock(&self.forwarder);
        let stream = take_stream(&self.slot);
        let was_open = stream.is_some();
        // Dropping the stream stops CPAL and disconnects the receiver; the
        // forwarder then drains the remaining frames and exits.
        drop(stream);
        if let Some(handle) = forwarder.take() {
            let _ = handle.join();
        }
        if was_open {
            crate::diag::log!("{LOG_PREFIX} capture closed");
        }
    }

    fn spawn_forwarder(&self, rx: PipelineReceiver) -> std::io::Result<JoinHandle<()>> {
        let frames = Arc::clone(&self.frames);
        let reporter = Arc::clone(&self.reporter);
        let health = Arc::clone(&self.health);
        let slot = Arc::clone(&self.slot);
        thread::Builder::new()
            .name("rust-session-audio".to_owned())
            .spawn(move || {
                let ForwardEnd::DeviceError(message) =
                    forward_frames(|| rx.recv().ok(), frames.as_ref())
                else {
                    return;
                };
                if lock(&slot).stopped {
                    return;
                }
                health.store(HEALTH_UNAVAILABLE, Ordering::Release);
                reporter.stderr(format!(
                    "{LOG_PREFIX} device error: {message}; microphone stays closed until the next push-to-talk press"
                ));
                reporter.capture_unavailable(&format!(
                    "Microphone capture stopped during recording; it will be reopened on the next push-to-talk press: {message}"
                ));
            })
    }

    fn report_opened(&self, target: OpenTarget, configured_error: Option<&str>, elapsed: Duration) {
        let label = target.effective_label(&self.configured);
        let fallback = target == OpenTarget::SystemDefault && has_named_device(&self.configured);
        let next = if fallback { HEALTH_FALLBACK } else { HEALTH_OK };
        let previous = self.health.swap(next, Ordering::AcqRel);
        if fallback && previous != HEALTH_FALLBACK {
            self.reporter.set_effective_device(label);
            self.reporter.stderr(format!(
                "{LOG_PREFIX} configured input unavailable ({}); using the system default input for this recording",
                configured_error.unwrap_or("unknown error")
            ));
            self.reporter
                .status("audio-fallback", Some(label), None, None);
        } else if !fallback && previous != HEALTH_OK {
            self.reporter.set_effective_device(label);
            self.reporter
                .status("audio-recovered", Some(label), None, None);
        }
        if elapsed >= self.slow_open_warning {
            self.reporter.stderr(format!(
                "{LOG_PREFIX} microphone took {} ms to open ({}); speech before it opened was not captured",
                elapsed.as_millis(),
                target.description()
            ));
        }
    }

    fn report_open_failure(&self, message: &str, paused: bool) {
        self.health.store(HEALTH_UNAVAILABLE, Ordering::Release);
        self.reporter
            .stderr(format!("{LOG_PREFIX} capture open failed: {message}"));
        self.reporter.capture_unavailable(&if paused {
            format!(
                "Microphone did not finish opening; capture is paused to avoid accumulating blocked audio-driver threads. Restart the dictation runtime to retry: {message}"
            )
        } else {
            format!(
                "Microphone could not be opened for this recording; it will be retried on the next push-to-talk press: {message}"
            )
        });
    }
}

impl<O: CaptureOpener, F: FrameSink> RecordingCapture for CaptureLifecycle<O, F> {
    fn open_for_recording(&self) {
        self.open();
    }

    fn close_for_recording(&self) {
        self.close();
    }
}

impl<O: CaptureOpener, F: FrameSink> Drop for CaptureLifecycle<O, F> {
    fn drop(&mut self) {
        stop_slot(&self.slot);
        let handle = self
            .forwarder
            .get_mut()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Publish a freshly opened stream unless teardown already began; a stop
/// that wins the race gets the stream closed here instead.
fn install_stream<S>(slot: &Mutex<Slot<S>>, stream: S) -> bool {
    let mut guard = lock(slot);
    if guard.stopped {
        drop(guard);
        drop(stream);
        return false;
    }
    guard.stream = Some(stream);
    true
}

fn take_stream<S>(slot: &Mutex<Slot<S>>) -> Option<S> {
    lock(slot).stream.take()
}

fn stop_slot<S>(slot: &Mutex<Slot<S>>) {
    let stream = {
        let mut guard = lock(slot);
        guard.stopped = true;
        guard.stream.take()
    };
    if stream.is_some() && crate::diag::debug_enabled() {
        crate::diag::log!("[runtime/debug] audio capture stop requested");
    }
    drop(stream);
}

#[cfg(test)]
#[path = "capture_lifecycle_tests.rs"]
mod tests;
