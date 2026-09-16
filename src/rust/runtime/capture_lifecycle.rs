//! Push-to-talk microphone lifecycle (#323): the capture device is open only
//! while recording.
//!
//! * Runtime start checks for an input by enumeration; no stream is opened,
//!   and a missing device is reported instead of refusing to start.
//! * [`RecordingCapture::open_for_recording`] opens the configured input (or
//!   the OS default for this recording when it fails) and spawns a forwarder
//!   thread that feeds every delivered frame into the session.
//! * [`RecordingCapture::close_for_recording`] closes the stream and joins the
//!   forwarder so the release tail reaches the session before transcription.
//! * A device error or reaching `max_record_s` closes the stream at once, so
//!   the OS microphone indicator goes off even though the recording itself
//!   ends only at the next release / toggle press.
//! * [`CaptureLifecycle::capture_stop`] and `Drop` close any open stream and
//!   refuse later opens; a stop that lands during an open prevents any
//!   further candidate from being opened and suppresses status reports.
//!
//! Nothing reopens a stream while idle.

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

/// Body of the frame-forwarder thread.
pub(crate) type ForwarderJob = Box<dyn FnOnce() + Send + 'static>;

/// Starts the forwarder thread. Injectable so a spawn failure is testable.
pub(crate) type ThreadSpawner =
    Arc<dyn Fn(ForwarderJob) -> std::io::Result<JoinHandle<()>> + Send + Sync>;

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
    spawn_thread: ThreadSpawner,
}

impl<O: CaptureOpener, F: FrameSink> CaptureLifecycle<O, F> {
    /// Check the configured input by enumeration. No stream is opened.
    pub(crate) fn new(
        opener: O,
        frames: Arc<F>,
        configured: &str,
        reporter: CaptureReporter,
    ) -> Self {
        let startup = probe_startup_device(configured, |selector| opener.probe(selector));
        let health = report_startup(&reporter, &startup);
        crate::diag::log!(
            "{LOG_PREFIX} input checked without opening a stream ({startup:?}); microphone stays closed until push-to-talk"
        );
        Self {
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
            spawn_thread: Arc::new(spawn_forwarder_thread),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_slow_open_warning(mut self, threshold: Duration) -> Self {
        self.slow_open_warning = threshold;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_thread_spawner(mut self, spawner: ThreadSpawner) -> Self {
        self.spawn_thread = spawner;
        self
    }

    /// Close callback for runtime teardown. Never waits for a recording,
    /// an in-flight open, or transcription.
    pub(crate) fn capture_stop(&self) -> CaptureStop {
        let slot = Arc::clone(&self.slot);
        Arc::new(move || stop_slot(&slot))
    }

    fn is_stopped(&self) -> bool {
        lock(&self.slot).stopped
    }

    fn open(&self) -> bool {
        let mut forwarder = lock(&self.forwarder);
        // Check teardown first: after `capture_stop` a finished forwarder may
        // still be registered, and that must not read as an open microphone.
        if self.is_stopped() {
            return false;
        }
        if forwarder.is_some() {
            crate::diag::log!("{LOG_PREFIX} capture already open for this recording");
            return true;
        }
        let started = Instant::now();
        let result = {
            let mut breaker = lock(&self.breaker);
            open_with_fallback(
                &self.configured,
                &mut breaker,
                |selector| {
                    // A stop that lands while an earlier candidate was
                    // opening must not open the next one.
                    if self.is_stopped() {
                        return Err(anyhow::anyhow!("capture stopped during open"));
                    }
                    self.opener.open(selector)
                },
                |error| self.opener.is_timeout(error),
            )
        };
        let opened = match result {
            Ok(opened) => opened,
            Err(failure) => {
                if self.is_stopped() {
                    crate::diag::log!("{LOG_PREFIX} capture open abandoned after runtime stop");
                } else {
                    self.report_open_failure(&failure.message, failure.paused);
                }
                return false;
            }
        };
        let elapsed = started.elapsed();
        let (stream, rx) = opened.stream;
        if !install_stream(&self.slot, stream) {
            crate::diag::log!("{LOG_PREFIX} capture opened after runtime stop; closed immediately");
            return false;
        }
        crate::diag::log!(
            "{LOG_PREFIX} capture opened in {} ms (target={})",
            elapsed.as_millis(),
            opened.target.description()
        );
        let spawned = match self.spawn_forwarder(rx) {
            Ok(handle) => handle,
            Err(error) => {
                drop(take_stream(&self.slot));
                // Unavailable, so the next successful open announces recovery
                // and clears the banner raised here.
                self.health.store(HEALTH_UNAVAILABLE, Ordering::Release);
                self.reporter.capture_unavailable(&format!(
                    "Microphone capture could not start its forwarding thread: {error}"
                ));
                return false;
            }
        };
        // Claim the open under the slot lock that `capture_stop` and the
        // forwarder's terminal path both take, and publish the success while
        // still holding it. Releasing between the claim and the report would
        // let a teardown -- or a forwarder that has already died of a device
        // error and taken the stream with it -- be overwritten by
        // `report_opened`, leaving the caller to announce a recording, play
        // the start cue and duck audio for a microphone already closed.
        let mut spawned = Some(spawned);
        let mut lost_to = None;
        {
            let guard = lock(&self.slot);
            if guard.stopped {
                lost_to = Some("runtime stop");
            } else if guard.stream.is_none() {
                lost_to = Some("the forwarder ending");
            } else {
                *forwarder = spawned.take();
                self.report_opened(opened.target, opened.configured_error.as_deref(), elapsed);
            }
        }
        if let Some(reason) = lost_to {
            drop(take_stream(&self.slot));
            if let Some(handle) = spawned.take() {
                let _ = handle.join();
            }
            crate::diag::log!(
                "{LOG_PREFIX} capture opened as {reason} landed; closed again without announcing a recording"
            );
            return false;
        }
        true
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
        (self.spawn_thread)(Box::new(move || {
            let end = forward_frames(|| rx.recv().ok(), frames.as_ref());
            finish_forwarding(end, &slot, &reporter, &health);
        }))
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
                "Microphone could not be opened for this recording; it will be retried when the next recording starts: {message}"
            )
        });
    }
}

impl<O: CaptureOpener, F: FrameSink> RecordingCapture for CaptureLifecycle<O, F> {
    fn open_for_recording(&self) -> bool {
        self.open()
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

fn spawn_forwarder_thread(job: ForwarderJob) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("rust-session-audio".to_owned())
        .spawn(job)
}

fn report_startup(reporter: &CaptureReporter, startup: &StartupDevice) -> u8 {
    match startup {
        StartupDevice::Configured | StartupDevice::SystemDefault => HEALTH_OK,
        StartupDevice::ConfiguredMissing { error } => {
            reporter.set_effective_device(SYSTEM_DEFAULT_LABEL);
            reporter.stderr(format!(
                "{LOG_PREFIX} configured input not found at startup ({error}); the system default input will be used when recording starts"
            ));
            HEALTH_STARTUP_FALLBACK
        }
        StartupDevice::NoInput { error } => {
            reporter.stderr(format!(
                "{LOG_PREFIX} no input device found at startup ({error}); the runtime starts anyway and tries again when push-to-talk is pressed"
            ));
            reporter.capture_unavailable(&format!(
                "No microphone was found. Connect one; it will be used when the next recording starts: {error}"
            ));
            HEALTH_UNAVAILABLE
        }
    }
}

/// After the forwarder ends early, close the device immediately so the OS
/// microphone indicator goes off; the recording ends at the next release.
fn finish_forwarding<S>(
    end: ForwardEnd,
    slot: &Mutex<Slot<S>>,
    reporter: &CaptureReporter,
    health: &AtomicU8,
) {
    if end == ForwardEnd::Closed {
        return;
    }
    let (stream, stopped) = {
        let mut guard = lock(slot);
        (guard.stream.take(), guard.stopped)
    };
    drop(stream);
    if stopped {
        return;
    }
    match end {
        ForwardEnd::DeviceError(message) => {
            health.store(HEALTH_UNAVAILABLE, Ordering::Release);
            reporter.stderr(format!(
                "{LOG_PREFIX} device error: {message}; microphone closed until the next recording starts"
            ));
            reporter.capture_unavailable(&format!(
                "Microphone capture stopped during recording; it will be reopened when the next recording starts (in toggle mode, after the current one ends): {message}"
            ));
        }
        ForwardEnd::RecordingFull => reporter.stderr(format!(
            "{LOG_PREFIX} recording reached max_record_s; microphone closed until the recording ends"
        )),
        ForwardEnd::Closed => {}
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
