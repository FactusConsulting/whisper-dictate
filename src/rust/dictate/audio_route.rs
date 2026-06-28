//! Routes [`AudioPipeline`](crate::audio::AudioPipeline) events into a
//! [`DictateSession`](crate::dictate::session::DictateSession).
//!
//! Wave 5 PR 3 of issue #348. The pipeline (cpal → resampler → Silero
//! VAD) produces a stream of [`PipelineEvent`](crate::audio::PipelineEvent)s
//! on a background thread; the supervisor (PR 4) pulls them off the
//! `mpsc::Receiver` and hands each one to [`AudioRoute::on_event`]. The
//! route applies the four behaviour gates that today live in
//! `src/python/whisper_dictate/vp_capture_rust_stdin.py` (the Python
//! receiver this Rust route eventually replaces):
//!
//! 1. **Idle-frame drop** (`vp_capture_rust_stdin.py:192-193`) — frames
//!    that arrive while the session is not in [`SessionState::Recording`]
//!    are dropped silently. The pipeline thread runs continuously between
//!    PTT presses; without this gate, stale audio captured before the
//!    next press would leak into the next utterance's buffer.
//! 2. **Max-record cap** (`vp_capture_rust_stdin.py:200-224`) — once the
//!    buffered duration exceeds the `VOICEPI_MAX_RECORD_S` cap, further
//!    frames are dropped AND an automatic
//!    [`DictateSession::stop_and_transcribe`] closes the recording so
//!    the user gets their text without releasing PTT. The Python path
//!    only refuses frames + emits a "capped" status event; we go a step
//!    further to match the sounddevice / arecord callbacks that DO
//!    auto-stop, keeping the three capture backends behaviour-consistent.
//! 3. **DeviceError surfacing** (`vp_capture_rust_stdin.py:233-236`) —
//!    a [`PipelineEvent::DeviceError`] emits an `event=error` worker
//!    event via [`crate::dictate::events::emit_error`] and returns
//!    [`RouteError::Device`]. The supervisor (PR 4/6) owns restart.
//! 4. **Cancelled passthrough** — a [`PipelineEvent::Cancelled`]
//!    (emitted by the VAD when an in-flight utterance is discarded)
//!    runs through [`DictateSession::cancel`] with the active epoch so
//!    the session settles back to [`SessionState::Idle`] and the buffer
//!    is dropped.
//!
//! The route is also responsible for translating
//! [`PipelineEvent::SpeechStart`] / [`PipelineEvent::SpeechEnd`] into
//! [`SpeechMarker`] return values; the supervisor in PR 4 wires those
//! into the preview / live-card UI.
//!
//! # Why a separate buffer-length counter
//!
//! The session already tracks its own `frame_buf`, but per-utterance
//! state is opaque to the session by design (it's purely a state
//! machine, see `session/mod.rs` module docs). The cap is a route
//! concern — it depends on how the audio pipeline pumps frames in, not
//! on what the session decides to do with them — so the route owns
//! [`AudioRoute::buffered_samples`] independently. Reset on every
//! [`AudioRoute::start_recording`] / [`AudioRoute::stop_recording`] /
//! cap-trip.
//!
//! # No production caller in this PR
//!
//! PR 3 lands the module + tests only; PR 4 wires the supervisor to
//! drive `on_event` off the pipeline receiver. Keeping the wiring
//! separate lets the four behaviour gates be unit-tested in full
//! isolation before any I/O orchestration lands.

use std::io::Write;

use serde_json::{json, Value};

use crate::audio::{AudioPipeline, PipelineEvent};
use crate::dictate::events::{self, WorkerStatus};
use crate::dictate::session::{
    DictateSession, InjectBackend, SessionError, SessionState, TranscribeBackend, UtteranceOutcome,
    SR,
};

/// Env var that caps a single recording's duration in seconds.
/// Mirrors `vp_capture._max_record_s`:
///
/// * unset / unparseable → [`DEFAULT_MAX_RECORD_S`] (`120` s),
/// * `"0"` (or any non-positive / non-finite value) → no cap,
/// * positive finite → that many seconds.
///
/// Live-reload-friendly: read at [`RouteConfig::from_env`] call time
/// so a config reload between presses takes effect on the next
/// recording without a process restart (matches the
/// `live: true` flag on the `min_record_seconds` / `max_record_s`
/// settings in `src/python/whisper_dictate/settings_schema.json`).
pub const MAX_RECORD_ENV: &str = "VOICEPI_MAX_RECORD_S";

/// Default cap in seconds when `VOICEPI_MAX_RECORD_S` is unset OR
/// unparseable. Matches the literal in
/// `src/python/whisper_dictate/vp_capture.py::_max_record_s`.
pub const DEFAULT_MAX_RECORD_S: f64 = 120.0;

/// Configuration knobs for the audio route. All optional — a default
/// route ([`RouteConfig::default`]) has no cap and pumps every frame
/// straight into the session (subject to the recording-state gate).
/// Use [`RouteConfig::from_env`] for the env-driven 120 s default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouteConfig {
    /// Hard ceiling on per-recording duration in seconds. `None` =
    /// no cap. Populated by [`RouteConfig::from_env`] from
    /// [`MAX_RECORD_ENV`]; tests usually construct directly.
    pub max_record_seconds: Option<f64>,
}

impl RouteConfig {
    /// Read the cap from the [`MAX_RECORD_ENV`] env var. Parse rules
    /// match `vp_capture._max_record_s`:
    ///
    /// * env unset OR unparseable → `Some(DEFAULT_MAX_RECORD_S)` (the
    ///   120 s Python default; on the Python side an unparseable
    ///   value also falls back to 120),
    /// * env set to a non-positive / non-finite value → `None` (cap
    ///   disabled — Python's `if cap > 0:` guard),
    /// * env set to a positive finite value → `Some(value)`.
    pub fn from_env() -> Self {
        let raw = std::env::var(MAX_RECORD_ENV).ok();
        // Mirror Python's `(os.environ.get(...) or "120").strip()`:
        // an absent variable AND an unparseable string both fall back
        // to the 120 s default. A successfully parsed non-positive
        // value (e.g. `"0"`) is a deliberate "disable the cap" signal.
        let parsed: f64 = match raw.as_deref().map(str::trim) {
            None => DEFAULT_MAX_RECORD_S,
            Some(s) => s.parse::<f64>().unwrap_or(DEFAULT_MAX_RECORD_S),
        };
        let max = Some(parsed).filter(|v| v.is_finite() && *v > 0.0);
        Self {
            max_record_seconds: max,
        }
    }
}

/// VAD speech onset / hangover marker. The route translates
/// [`PipelineEvent::SpeechStart`] / [`PipelineEvent::SpeechEnd`] into
/// these so the supervisor in PR 4 can drive the live-preview /
/// utterance-card UI without re-matching on the raw pipeline event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechMarker {
    Start,
    End,
}

/// Errors a single [`AudioRoute::on_event`] call (or one of the
/// supervisor-facing helpers) can surface.
#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    /// Pipeline reported an unrecoverable capture failure. The route
    /// has already emitted an `event=error` worker event; the
    /// supervisor (PR 4/6) is responsible for restart / user-facing
    /// recovery.
    #[error("audio device error: {0}")]
    Device(String),
    /// The wrapped session refused a transition (e.g. duplicate
    /// `start_recording`).
    #[error(transparent)]
    Session(#[from] SessionError),
    /// An I/O write to the event-line writer failed. Distinct from
    /// `Device` so the supervisor can distinguish "the mic blew up"
    /// from "stderr is plugged".
    #[error("event writer I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for RouteError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

/// Owns a live [`AudioPipeline`] + a [`DictateSession`] and routes
/// pipeline events into the session. See the module docs for the four
/// behaviour gates.
///
/// The `pipeline` field is `Option<…>` so the test suite can construct
/// a route via `AudioRoute::for_test` without spinning up cpal; the
/// public [`AudioRoute::new`] constructor always populates it.
pub struct AudioRoute<T: TranscribeBackend, I: InjectBackend> {
    /// Live pipeline kept alive for as long as the route is. Dropped
    /// (and therefore stopped — see `AudioPipeline`'s `Drop` impl) when
    /// the route is dropped.
    pipeline: Option<AudioPipeline>,
    session: DictateSession<T, I>,
    config: RouteConfig,
    /// Total 16 kHz mono samples currently buffered for the in-flight
    /// recording. Independent of the session's own `frame_buf` —
    /// see the module docs.
    buffered_samples: usize,
    /// True once the max-record cap has fired for the current
    /// recording. Reset on `start_recording`. Prevents repeated cap
    /// trips on a single utterance.
    cap_tripped: bool,
}

impl<T: TranscribeBackend, I: InjectBackend> AudioRoute<T, I> {
    /// Build a route around a live pipeline + a session.
    pub fn new(
        pipeline: AudioPipeline,
        session: DictateSession<T, I>,
        config: RouteConfig,
    ) -> Self {
        Self {
            pipeline: Some(pipeline),
            session,
            config,
            buffered_samples: 0,
            cap_tripped: false,
        }
    }

    /// Test-only constructor that omits the pipeline. Tests drive
    /// `on_event` directly, so they don't need a real cpal stream.
    #[cfg(test)]
    pub(crate) fn for_test(session: DictateSession<T, I>, config: RouteConfig) -> Self {
        Self {
            pipeline: None,
            session,
            config,
            buffered_samples: 0,
            cap_tripped: false,
        }
    }

    /// Read-only access to the wrapped session. Lets the supervisor
    /// (PR 4) inspect state / epoch without re-borrowing the route.
    pub fn session(&self) -> &DictateSession<T, I> {
        &self.session
    }

    /// Current per-recording buffered-sample count. Tests assert this;
    /// the supervisor may read it for UI / telemetry.
    pub fn buffered_samples(&self) -> usize {
        self.buffered_samples
    }

    /// True once the in-flight recording tripped the max-record cap.
    /// Cleared on the next [`Self::start_recording`].
    pub fn cap_tripped(&self) -> bool {
        self.cap_tripped
    }

    /// Stop the underlying pipeline (no-op when running in test mode
    /// without a pipeline). Idempotent — safe to call multiple times.
    pub fn stop_pipeline(&mut self) {
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.stop();
        }
    }

    /// Open a fresh utterance. Delegates to [`DictateSession::start`]
    /// and resets the cap-tracking state. Returns the new recording
    /// epoch so the caller can stash it for a later
    /// [`DictateSession::cancel`].
    pub fn start_recording<W: Write>(&mut self, writer: &mut W) -> Result<u64, RouteError> {
        let id = self.session.start(writer)?;
        self.buffered_samples = 0;
        self.cap_tripped = false;
        Ok(id)
    }

    /// Close the in-flight utterance, decide skip / inject, and reset
    /// the buffered-sample counter. Delegates to
    /// [`DictateSession::stop_and_transcribe`].
    ///
    /// Note: [`Self::cap_tripped`] is **not** cleared here — it
    /// survives until the next [`Self::start_recording`] so the
    /// supervisor (PR 4) can distinguish a normal release-stop from
    /// the auto-stop that fired because the cap tripped. The buffered-
    /// sample counter, however, IS cleared so a stray
    /// `on_event(Frame)` arriving in the Idle gap doesn't accumulate
    /// against the next recording's cap.
    pub fn stop_recording<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, RouteError> {
        let outcome = self.session.stop_and_transcribe(writer)?;
        self.buffered_samples = 0;
        Ok(outcome)
    }

    /// Drive a single [`PipelineEvent`] into the session. Returns
    /// `Ok(Some(SpeechMarker))` for `SpeechStart` / `SpeechEnd` so the
    /// supervisor can pass the marker through to the live-preview UI;
    /// `Ok(None)` for every other branch. See the module docs for the
    /// behaviour gates.
    pub fn on_event<W: Write>(
        &mut self,
        event: PipelineEvent,
        writer: &mut W,
    ) -> Result<Option<SpeechMarker>, RouteError> {
        match event {
            PipelineEvent::Frame(frame) => {
                self.handle_frame(&frame, writer)?;
                Ok(None)
            }
            PipelineEvent::SpeechStart => Ok(Some(SpeechMarker::Start)),
            PipelineEvent::SpeechEnd => Ok(Some(SpeechMarker::End)),
            PipelineEvent::Cancelled => {
                // Use the session's current epoch — by the time we
                // observe Cancelled, the recording it refers to is the
                // current one (the pipeline emits Cancelled strictly
                // between SpeechStart and any would-be SpeechEnd, so
                // there is no race with a fresh start arriving from
                // the supervisor in this PR; the chord-race guard in
                // `DictateSession::cancel` is the load-bearing check
                // for the PTT cancel path that PR 4 wires).
                let epoch = self.session.epoch();
                self.session.cancel(epoch, writer)?;
                self.buffered_samples = 0;
                self.cap_tripped = false;
                Ok(None)
            }
            PipelineEvent::DeviceError(msg) => {
                self.emit_device_error(&msg, writer);
                Err(RouteError::Device(msg))
            }
        }
    }

    /// The `Frame` branch of [`Self::on_event`] — split out so the
    /// outer match stays scannable and the three frame-disposition
    /// outcomes (drop-idle / cap-trip-auto-stop / accept) read top-to-
    /// bottom.
    fn handle_frame<W: Write>(&mut self, frame: &[f32], writer: &mut W) -> Result<(), RouteError> {
        // Idle-frame drop. Mirrors vp_capture_rust_stdin.py:192-193.
        // Also catches the post-auto-stop window where `cap_tripped`
        // is set but the session is back in Idle (auto-stop drove the
        // transition); the state check is therefore the sole gate.
        if !matches!(self.session.state(), SessionState::Recording { .. }) {
            return Ok(());
        }
        // Max-record cap. Mirrors vp_capture_rust_stdin.py:200-224.
        if let Some(cap) = self.config.max_record_seconds {
            let buffered_with_frame = self.buffered_samples.saturating_add(frame.len());
            if (buffered_with_frame as f64 / f64::from(SR)) > cap {
                self.cap_tripped = true;
                // Auto-stop. Drops the trip-frame (matching Python's
                // `return True` before the append) but flushes the
                // already-buffered audio through the session so the
                // user gets their text. PR 4 will surface the
                // `capped` worker event the Python path emits before
                // refusing the frame; in this PR the stop_and_transcribe
                // call already drives the recording → transcribing →
                // (utterance | no_text) → ready transition that the UI
                // keys on.
                self.stop_recording(writer)?;
                return Ok(());
            }
        }
        self.session.push_frame(frame);
        self.buffered_samples = self.buffered_samples.saturating_add(frame.len());
        Ok(())
    }

    /// Emit the `event=error` worker line for a [`PipelineEvent::DeviceError`].
    /// Swallows a writer I/O failure on purpose: the device error itself
    /// is already the headline diagnostic the supervisor will surface,
    /// and we shouldn't mask it behind a follow-up "couldn't write the
    /// error event" failure.
    fn emit_device_error<W: Write>(&self, message: &str, writer: &mut W) {
        let payload: Value = json!({
            "state": WorkerStatus::Error.as_wire_str(),
            "backend": "rust-stdin",
        });
        let _ = events::emit_error(writer, message, &payload);
    }
}
