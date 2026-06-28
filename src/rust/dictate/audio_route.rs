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
//!    frames are dropped AND a one-shot `status=recording capped=true
//!    recording_s=N` worker event is emitted. We **do not** auto-stop
//!    the recording: the Python path also keeps the recording open
//!    after the cap fires (see `vp_capture._cb` / `_arecord_reader`),
//!    leaving the stop/transcribe path to the PTT-release handler.
//!    Auto-stopping mid-press would inject text while the PTT modifier
//!    is still down — which for bare-modifier bindings (Ctrl, Alt) would
//!    trigger keyboard shortcuts on the injected characters. The
//!    supervisor (PR 4) closes the recording on key-up as normal.
//! 3. **DeviceError surfacing** (`vp_capture_rust_stdin.py:233-236`) —
//!    a [`PipelineEvent::DeviceError`] emits a `status=capture_lost`
//!    worker event via [`crate::dictate::events::emit_status`] (the
//!    canonical state the Rust UI dispatcher in `src/rust/ui/app.rs`
//!    already handles — `event=error` would be ignored) and returns
//!    [`RouteError::Device`]. The supervisor (PR 4/6) owns restart.
//! 4. **Cancelled — diagnostic only** — a [`PipelineEvent::Cancelled`]
//!    (emitted by the VAD when an in-flight utterance is discarded)
//!    is currently dropped silently, matching Python's Phase-1
//!    behaviour (`vp_capture_rust_stdin.py:228-232`): the PTT-release
//!    boundary, not the VAD's, owns utterance closing. Routing it
//!    through `DictateSession::cancel` would race the chord-cancel
//!    epoch guard — the pipeline event carries no recording id, so
//!    a stale Cancelled queued from a prior recording could discard
//!    the next press's audio. PR 4/5 will revisit when the Rust VAD
//!    actually drives utterance commits.
//!
//! The route is also responsible for translating
//! [`PipelineEvent::SpeechStart`] / [`PipelineEvent::SpeechEnd`] into
//! [`SpeechMarker`] return values **while a recording is in flight**;
//! markers heard while the session is idle are dropped so background
//! speech between PTT presses doesn't trip stale UI transitions in the
//! live-preview / utterance-card consumers the supervisor (PR 4) wires.
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

use serde_json::{Map, Value};

use crate::audio::{AudioPipeline, PipelineEvent};
use crate::dictate::events::{self, StatusEvent, WorkerStatus};
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
    /// has already emitted a `status=capture_lost` worker event; the
    /// supervisor (PR 4/6) is responsible for restart / user-facing
    /// recovery.
    #[error("audio device error: {0}")]
    Device(String),
    /// The wrapped session refused a transition (e.g. duplicate
    /// `start_recording`). I/O failures from the session writer are
    /// re-routed to [`RouteError::Io`] below so supervisors can split
    /// "transition refused" from "stderr is plugged"; only true
    /// transition errors (AlreadyActive etc.) land in this variant.
    /// Codex P2 #415 audio_route.rs:162.
    #[error(transparent)]
    Session(SessionError),
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

impl From<SessionError> for RouteError {
    fn from(value: SessionError) -> Self {
        // The session funnels both writer errors AND duplicate-start /
        // bad-state refusals through the same enum; the supervisor (PR 4/6)
        // wants them in different buckets so it can fail-loud on the
        // transition errors while recovering from a pipe blowup. Map the
        // I/O case onto the dedicated Io variant; pass everything else
        // through as Session. Codex P2 #415 audio_route.rs:162.
        match value {
            SessionError::Io(msg) => Self::Io(msg),
            other => Self::Session(other),
        }
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

    /// Current recording epoch (delegates to [`DictateSession::epoch`]).
    /// Supervisors stamp outgoing cancel requests with this so the
    /// session's stale-cancel guard fires correctly under chord races.
    pub fn epoch(&self) -> u64 {
        self.session.epoch()
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

    /// Open a fresh utterance. Delegates to [`DictateSession::start`],
    /// resets the cap-tracking state, AND re-reads
    /// [`MAX_RECORD_ENV`] so a Settings save between PTT presses takes
    /// effect on the next recording without rebuilding the route — the
    /// `max_record_s` setting is `live: true` in
    /// `src/python/whisper_dictate/settings_schema.json`, and the
    /// Python capture callbacks re-read `_max_record_s()` per
    /// recording. Codex P2 #415 audio_route.rs:250.
    ///
    /// Returns the new recording epoch so the caller can stash it for
    /// a later [`DictateSession::cancel`].
    pub fn start_recording<W: Write>(&mut self, writer: &mut W) -> Result<u64, RouteError> {
        self.config = RouteConfig::from_env();
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

    /// Cancel the in-flight recording if `requested_epoch` matches the
    /// session's current epoch. Drops the buffered audio, settles back
    /// to Idle, and emits `status=cancelled` then `status=ready`
    /// through the writer. Delegates to [`DictateSession::cancel`] --
    /// the session's stale-cancel guard (`vp_dictate.py:140-147 +
    /// 665-684`) is what makes this safe under PTT chord races.
    ///
    /// Without this accessor, a supervisor that owns `AudioRoute` had
    /// no way to honour a chord-cancel: `session` is read-only via
    /// [`Self::session`], and falling back to [`Self::stop_recording`]
    /// would transcribe + inject text the user cancelled. Codex P2
    /// #415 audio_route.rs:251.
    pub fn cancel<W: Write>(
        &mut self,
        requested_epoch: u64,
        writer: &mut W,
    ) -> Result<(), RouteError> {
        // Capture the active epoch BEFORE calling cancel so we can tell
        // whether the session actually dropped the recording (epoch
        // matched) or no-oped (stale epoch). Only clear our own
        // buffered_samples counter when the session actually cancelled
        // -- otherwise a stale cancel from a prior press would
        // additionally erase the NEW recording's sample count even
        // though the session correctly preserved its buffer.
        let active_before = self.session.epoch();
        self.session.cancel(requested_epoch, writer)?;
        if requested_epoch == active_before {
            self.buffered_samples = 0;
        }
        Ok(())
    }

    /// Drop any frames the supervisor has not yet drained from the
    /// pipeline receiver. Called by the supervisor BETWEEN
    /// [`Self::stop_recording`] and the next [`Self::start_recording`]
    /// to prevent stale frames from the previous press leaking into
    /// the new recording (Codex P2 #415 audio_route.rs:398).
    ///
    /// `PipelineEvent::Frame` carries no recording id and the
    /// idle-frame drop in [`Self::handle_frame`] only protects frames
    /// processed BEFORE the start handshake; frames queued in the
    /// channel before `start_recording()` but processed after it would
    /// otherwise land in the new utterance. The supervisor (PR 4) is
    /// the only component that owns the channel, so it is the only
    /// component that can drain it; this method is a documentation
    /// hook plus a no-op reset of internal counters so the contract
    /// is auditable. The real drain happens in the supervisor's PTT
    /// release handler.
    pub fn fence_pending_frames(&mut self) {
        // Internal counters are already cleared on start_recording, so
        // the body is intentionally empty -- the value is the doc
        // contract above. Kept as a real method so tests + PR 4 wiring
        // can reference an actual symbol.
        // If a future refactor adds per-recording fencing state
        // (e.g. a "pending stale frames" buffer), this is the place
        // to drop it.
    }

    /// Drive a single [`PipelineEvent`] into the session. Returns
    /// `Ok(Some(SpeechMarker))` for `SpeechStart` / `SpeechEnd` heard
    /// **while a recording is in flight**; `Ok(None)` for every other
    /// branch (including speech markers heard while idle — see Codex
    /// P2 #415 audio_route.rs:290). See the module docs for the four
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
            PipelineEvent::SpeechStart => Ok(self.speech_marker_if_recording(SpeechMarker::Start)),
            PipelineEvent::SpeechEnd => Ok(self.speech_marker_if_recording(SpeechMarker::End)),
            PipelineEvent::Cancelled => {
                // Drop silently. The pipeline event carries no
                // recording id, so routing it through
                // `DictateSession::cancel` would race the chord-cancel
                // epoch guard — a stale Cancelled queued from a prior
                // recording could discard the new utterance. Python's
                // Phase-1 rust-stdin handler also ignores Cancelled
                // (vp_capture_rust_stdin.py:228-232); the PTT-release
                // path is the authoritative cancel trigger. PR 4/5
                // will revisit when the Rust VAD drives commits.
                // Codex P2 #415 audio_route.rs:300.
                Ok(None)
            }
            PipelineEvent::DeviceError(msg) => {
                self.emit_device_error(&msg, writer);
                // Tear down the cpal worker + stream BEFORE returning
                // the device error. The pipeline pump may already have
                // exited, but `CaptureHandle` only drops the underlying
                // resources on `AudioPipeline::stop` / drop -- without
                // this call, a supervisor that handles `RouteError::Device`
                // by constructing a replacement route (rather than
                // dropping this one) would leak the old capture thread +
                // mic stream. `stop_pipeline` is idempotent and a no-op
                // in test mode (no pipeline). Codex P2 #415 audio_route.rs:327.
                self.stop_pipeline();
                Err(RouteError::Device(msg))
            }
        }
    }

    /// Speech-marker gate: only surface SpeechStart/SpeechEnd when a
    /// recording is in flight. Idle background speech between PTT
    /// presses would otherwise drive stale UI transitions. Codex P2
    /// #415 audio_route.rs:290.
    fn speech_marker_if_recording(&self, marker: SpeechMarker) -> Option<SpeechMarker> {
        if matches!(self.session.state(), SessionState::Recording { .. }) {
            Some(marker)
        } else {
            None
        }
    }

    /// The `Frame` branch of [`Self::on_event`] — split out so the
    /// outer match stays scannable and the three frame-disposition
    /// outcomes (drop-idle / drop-over-cap / accept) read top-to-bottom.
    fn handle_frame<W: Write>(&mut self, frame: &[f32], writer: &mut W) -> Result<(), RouteError> {
        // Idle-frame drop. Mirrors vp_capture_rust_stdin.py:192-193.
        if !matches!(self.session.state(), SessionState::Recording { .. }) {
            return Ok(());
        }
        // Max-record cap. Mirrors vp_capture_rust_stdin.py:200-224
        // exactly: refuse over-cap frames, emit the one-shot
        // `capped=true` status event on the FIRST trip, then keep the
        // recording open until the PTT-release handler closes it.
        // Auto-stopping mid-press would inject text while modifiers
        // are still held — Codex P2 #415 audio_route.rs:339.
        if let Some(cap) = self.config.max_record_seconds {
            let buffered_with_frame = self.buffered_samples.saturating_add(frame.len());
            let buffered_s = buffered_with_frame as f64 / f64::from(SR);
            if buffered_s > cap {
                if !self.cap_tripped {
                    self.cap_tripped = true;
                    self.emit_capped_status(buffered_s, writer)?;
                }
                return Ok(());
            }
        }
        self.session.push_frame(frame);
        self.buffered_samples = self.buffered_samples.saturating_add(frame.len());
        Ok(())
    }

    /// One-shot `status=recording capped=true recording_s=N` worker
    /// event, mirroring `vp_capture_rust_stdin.py:212-224`'s
    /// `_emit_worker_event("status", state="recording", capped=True,
    /// recording_s=round(buffered_s, 1))`. `recording_s` is rounded to
    /// one decimal to match Python.
    fn emit_capped_status<W: Write>(
        &self,
        buffered_s: f64,
        writer: &mut W,
    ) -> Result<(), RouteError> {
        let mut extras = Map::new();
        extras.insert("capped".into(), Value::from(true));
        extras.insert("recording_s".into(), Value::from(round_to_1dp(buffered_s)));
        let event = StatusEvent {
            state: WorkerStatus::Recording,
            extras,
            ..StatusEvent::new(WorkerStatus::Recording)
        };
        events::emit_status(writer, &event)?;
        Ok(())
    }

    /// Emit the `status=capture_lost` worker line for a
    /// [`PipelineEvent::DeviceError`]. We use the canonical
    /// `WorkerStatus::CaptureLost` state — the Rust UI dispatcher in
    /// `src/rust/ui/app.rs` switches on `event=status` and handles
    /// `state=capture_lost` specifically; an `event=error` line would
    /// be parsed and then ignored. Codex P2 #415 audio_route.rs:358.
    ///
    /// Swallows a writer I/O failure on purpose: the device error
    /// itself is already the headline diagnostic the supervisor will
    /// surface, and we shouldn't mask it behind a follow-up
    /// "couldn't write the status line" failure.
    fn emit_device_error<W: Write>(&self, message: &str, writer: &mut W) {
        let mut extras = Map::new();
        // The Rust UI's status logger (`src/rust/ui/worker_event.rs:12-24`)
        // forwards a fixed allowlist of fields onto the status card.
        // `reason` is on the list; `message` is NOT, so writing the
        // actionable text only under `message` would silently reduce
        // every mic/VAD failure to a generic "capture_lost" line in the
        // log. Put the text under `reason` (UI-consumed) and ALSO
        // duplicate it under `message` so any consumer that greps the
        // raw worker-event stream for the original field keeps working.
        // Codex P2 #415 audio_route.rs:409.
        extras.insert("reason".into(), Value::from(message));
        extras.insert("message".into(), Value::from(message));
        extras.insert("backend".into(), Value::from("rust-stdin"));
        let event = StatusEvent {
            state: WorkerStatus::CaptureLost,
            extras,
            ..StatusEvent::new(WorkerStatus::CaptureLost)
        };
        let _ = events::emit_status(writer, &event);
    }
}

/// Round to one decimal place, matching Python's
/// `round(buffered_s, 1)` in `vp_capture_rust_stdin.py:222`. Kept
/// local rather than importing `session::wire::round2` to avoid
/// reaching into a sibling module's private helper for a one-liner.
fn round_to_1dp(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}
