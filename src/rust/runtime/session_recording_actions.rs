//! Recording actions the coordinator sink performs on the dictation session,
//! split out of [`super::rust_session_sink`] for the push-to-talk microphone
//! lifecycle (#323).
//!
//! With native capture the order is:
//!
//! 1. **start** -- `begin_recording` (buffering on), release the session,
//!    open the microphone, then `announce_recording` (`status=recording`,
//!    start cue, ducking) only once capture is live. If no microphone could
//!    be opened the recording is abandoned without a start cue and the caller
//!    ends the coordinator's cycle, so the next press opens again.
//! 2. **stop** -- keep capturing through `release_tail_ms`, close the
//!    microphone (every tail frame delivered), then transcribe.
//! 3. **cancel** -- close the microphone, then discard.
//!
//! A panic inside an action closes the microphone while unwinding.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::live_settings::LiveEnvOverrides;
use super::recording_capture::{self, close_on_unwind, RecordingCaptureHandle};
use super::rust_session_sink::EventForwarder;
use crate::dictate::{DictateSession, InjectBackend, SessionState, TranscribeBackend};
use crate::runtime::{RepaintNotifier, RuntimeEvent};

/// Release tail used until the first live-settings reload.
const DEFAULT_RELEASE_TAIL: Duration = Duration::from_millis(200);

pub(super) struct RecordingActions<T: TranscribeBackend, I: InjectBackend> {
    session: Arc<Mutex<DictateSession<T, I>>>,
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
    live_env_overrides: LiveEnvOverrides,
    runtime_boundaries: bool,
    recording_capture: Option<RecordingCaptureHandle>,
    release_tail: Duration,
}

impl<T, I> RecordingActions<T, I>
where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
{
    pub(super) fn new(
        session: Arc<Mutex<DictateSession<T, I>>>,
        tx: Sender<RuntimeEvent>,
        repaint_notifier: Option<RepaintNotifier>,
        live_env_overrides: LiveEnvOverrides,
        runtime_boundaries: bool,
        recording_capture: Option<RecordingCaptureHandle>,
    ) -> Self {
        Self {
            session,
            tx,
            repaint_notifier,
            live_env_overrides,
            runtime_boundaries,
            recording_capture,
            release_tail: DEFAULT_RELEASE_TAIL,
        }
    }

    /// Start a recording. Returns `true` when the recording was begun but
    /// then abandoned because no microphone could be opened (or announcing
    /// it failed); the caller must end the coordinator's recording cycle.
    pub(super) fn start(&mut self, id: u64) -> bool {
        let capture = self.recording_capture.clone();
        let _close_on_panic = close_on_unwind(capture.as_ref());
        let session = Arc::clone(&self.session);
        let mut guard = lock(&session);
        self.reload_live_settings(&mut guard);
        let mut events = EventForwarder::new(&self.tx, self.repaint_notifier.as_ref());
        let begun = if capture.is_some() {
            guard.begin_recording(&mut events)
        } else {
            guard.start(&mut events)
        };
        if let Err(err) = begun {
            self.report_failure("start", id, &err);
            return false;
        }
        if crate::diag::debug_enabled() {
            crate::diag::log!("[dispatch] session_start emitted coord_id={id}");
        }
        if capture.is_none() {
            return false;
        }
        // Release the session so the capture forwarder can buffer the very
        // first frame the device delivers.
        drop(events);
        drop(guard);
        let opened = recording_capture::open(capture.as_ref());
        let mut guard = lock(&session);
        let mut events = EventForwarder::new(&self.tx, self.repaint_notifier.as_ref());
        let settled = if opened {
            guard.announce_recording(&mut events)
        } else {
            guard.abandon_recording(&mut events)
        };
        drop(events);
        drop(guard);
        let abandoned = !opened || settled.is_err();
        if let Err(err) = settled {
            recording_capture::close(capture.as_ref());
            self.report_failure("start", id, &err);
        }
        abandoned
    }

    pub(super) fn stop(&mut self, id: u64) {
        let capture = self.recording_capture.clone();
        let _close_on_panic = close_on_unwind(capture.as_ref());
        let session = Arc::clone(&self.session);
        if self.runtime_boundaries {
            // Python reloads at the top of `_stop_and_transcribe`, then keeps
            // capture open for release_tail_ms with the session unlocked so
            // the forwarder can append tail frames.
            self.reload_live_settings(&mut lock(&session));
            if !self.release_tail.is_zero() {
                std::thread::sleep(self.release_tail);
            }
        }
        // Closing joins the forwarder, so every tail frame is in the session
        // before transcription runs with the microphone closed.
        recording_capture::close(capture.as_ref());
        let mut guard = lock(&session);
        let mut events = EventForwarder::new(&self.tx, self.repaint_notifier.as_ref());
        let outcome = guard.stop_and_transcribe(&mut events);
        drop(guard);
        drop(events);
        match outcome {
            Ok(_) => {
                if crate::diag::debug_enabled() {
                    crate::diag::log!("[dispatch] session_stop emitted coord_id={id}");
                }
            }
            Err(err) => self.report_failure("stop", id, &err),
        }
    }

    pub(super) fn cancel(&mut self, id: u64) {
        let session = Arc::clone(&self.session);
        // A stale cancel (an epoch the session will reject) must not close the
        // microphone of the recording that is actually running.
        if cancel_targets_active_recording(lock(&session).state(), id) {
            recording_capture::close(self.recording_capture.as_ref());
        }
        let mut guard = lock(&session);
        let mut events = EventForwarder::new(&self.tx, self.repaint_notifier.as_ref());
        let result = guard.cancel(id, &mut events);
        drop(guard);
        drop(events);
        match result {
            Ok(()) => {
                if crate::diag::debug_enabled() {
                    crate::diag::log!("[dispatch] session_cancel emitted coord_id={id}");
                }
            }
            Err(err) => self.report_failure("cancel", id, &err),
        }
    }

    fn reload_live_settings(&mut self, session: &mut DictateSession<T, I>) {
        if !self.runtime_boundaries {
            return;
        }
        match super::live_settings::reload(session, &self.live_env_overrides) {
            Ok(tail) => self.release_tail = tail,
            Err(err) => report_live_reload_failure(&self.tx, self.repaint_notifier.as_ref(), &err),
        }
    }

    fn report_failure(&self, verb: &str, id: u64, err: &dyn std::fmt::Display) {
        if crate::diag::debug_enabled() {
            crate::diag::log!("[dispatch] session_{verb} refused coord_id={id} reason={err}");
        }
        let _ = self.tx.send(RuntimeEvent::Error(format!(
            "[rust-session] {verb} failed (coord id={id}): {err}"
        )));
    }
}

fn lock<S>(mutex: &Mutex<S>) -> MutexGuard<'_, S> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// True when `requested` is the epoch of the recording currently in flight.
/// [`DictateSession::cancel`] silently ignores any other epoch, so capture
/// must stay open for those.
fn cancel_targets_active_recording(state: SessionState, requested: u64) -> bool {
    matches!(
        state,
        SessionState::Recording { id } | SessionState::Opening { id } if id == requested
    )
}

fn report_live_reload_failure(
    tx: &Sender<RuntimeEvent>,
    repaint_notifier: Option<&RepaintNotifier>,
    err: &str,
) {
    let message = format!("[runtime] {err}; retaining last-good session settings");
    crate::diag::log!("{message}");
    let _ = tx.send(RuntimeEvent::Stderr(message));
    if let Some(notifier) = repaint_notifier {
        notifier();
    }
}

#[cfg(all(test, feature = "audio-capture"))]
#[path = "session_recording_actions_tests.rs"]
mod tests;
