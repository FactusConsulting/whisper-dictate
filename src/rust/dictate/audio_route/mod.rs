//! Routes [`AudioPipeline`](crate::audio::AudioPipeline) events into a
//! [`DictateSession`](crate::dictate::session::DictateSession).
//!
//! Wave 5 PR 3 of issue #348. The pipeline (cpal -> resampler -> Silero
//! VAD) produces a stream of [`PipelineEvent`](crate::audio::PipelineEvent)s
//! on a background thread; the supervisor (PR 4) pulls them off the
//! `mpsc::Receiver` and hands each one to [`AudioRoute::on_event`]. The
//! route applies the four behaviour gates that today live in
//! `src/python/whisper_dictate/vp_capture_rust_stdin.py` (the Python
//! receiver this Rust route eventually replaces):
//!
//! 1. **Idle-frame drop** (`vp_capture_rust_stdin.py:192-193`) -- frames
//!    that arrive while the session is not in [`SessionState::Recording`]
//!    are dropped silently. The pipeline thread runs continuously between
//!    PTT presses; without this gate, stale audio captured before the
//!    next press would leak into the next utterance buffer.
//! 2. **Max-record cap** (`vp_capture_rust_stdin.py:200-224`) -- once the
//!    buffered duration exceeds the `VOICEPI_MAX_RECORD_S` cap, further
//!    frames are dropped AND a one-shot `status=recording capped=true
//!    recording_s=N` worker event is emitted. We **do not** auto-stop
//!    the recording: the Python path also keeps the recording open
//!    after the cap fires (see `vp_capture._cb` / `_arecord_reader`),
//!    leaving the stop/transcribe path to the PTT-release handler.
//!    Auto-stopping mid-press would inject text while the PTT modifier
//!    is still down -- which for bare-modifier bindings (Ctrl, Alt) would
//!    trigger keyboard shortcuts on the injected characters. The
//!    supervisor (PR 4) closes the recording on key-up as normal.
//! 3. **DeviceError surfacing** (`vp_capture_rust_stdin.py:233-236`) --
//!    a [`PipelineEvent::DeviceError`] emits a `status=capture_lost`
//!    worker event via [`crate::dictate::events::emit_status`] (the
//!    canonical state the Rust UI dispatcher in `src/rust/ui/app.rs`
//!    already handles -- `event=error` would be ignored) and returns
//!    [`RouteError::Device`]. The supervisor (PR 4/6) owns restart.
//! 4. **Cancelled -- diagnostic only** -- a [`PipelineEvent::Cancelled`]
//!    (emitted by the VAD when an in-flight utterance is discarded)
//!    is currently dropped silently, matching Python Phase-1
//!    behaviour (`vp_capture_rust_stdin.py:228-232`): the PTT-release
//!    boundary, not the VAD, owns utterance closing. Routing it
//!    through `DictateSession::cancel` would race the chord-cancel
//!    epoch guard -- the pipeline event carries no recording id, so
//!    a stale Cancelled queued from a prior recording could discard
//!    the next press audio. PR 4/5 will revisit when the Rust VAD
//!    actually drives utterance commits.
//!
//! The route is also responsible for translating
//! [`PipelineEvent::SpeechStart`] / [`PipelineEvent::SpeechEnd`] into
//! [`SpeechMarker`] return values **while a recording is in flight**;
//! markers heard while the session is idle are dropped so background
//! speech between PTT presses does not trip stale UI transitions in the
//! live-preview / utterance-card consumers the supervisor (PR 4) wires.
//!
//! # Module layout (Codex P2 #415 audio_route.rs:530, round 7-B)
//!
//! Split into three siblings so each file stays under the AGENTS.md
//! ~500 LOC bar:
//!
//! * `mod.rs` (this file) -- `AudioRoute` struct + public API +
//!   `RouteError`, `SpeechMarker`.
//! * [`config`] -- `RouteConfig`, env-var constants, and parsers.
//! * [`events`] -- the `emit_capped_status` / `emit_device_error`
//!   helpers and the `round_to_1dp` rounder.

use std::io::Write;

use crate::audio::{AudioPipeline, PipelineEvent};
use crate::dictate::session::{
    DictateSession, InjectBackend, SessionError, SessionState, TranscribeBackend, UtteranceOutcome,
    SR,
};

pub mod config;
mod events;

pub use config::{
    RouteConfig, DEFAULT_MAX_RECORD_S, DEFAULT_MIN_RECORD_S, MAX_RECORD_ENV, MIN_RECORD_ENV,
};

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
/// The `pipeline` field is `Option<...>` so the test suite can construct
/// a route via `AudioRoute::for_test` without spinning up cpal; the
/// public [`AudioRoute::new`] constructor always populates it.
pub struct AudioRoute<T: TranscribeBackend, I: InjectBackend> {
    /// Live pipeline kept alive for as long as the route is. Dropped
    /// (and therefore stopped -- see `AudioPipeline` `Drop` impl) when
    /// the route is dropped.
    pipeline: Option<AudioPipeline>,
    session: DictateSession<T, I>,
    config: RouteConfig,
    /// Total 16 kHz mono samples currently buffered for the in-flight
    /// recording. Independent of the session own `frame_buf`.
    buffered_samples: usize,
    /// True once the max-record cap has fired for the current
    /// recording. Reset on `start_recording`. Prevents repeated cap
    /// trips on a single utterance.
    cap_tripped: bool,
    /// Monotonic counter bumped on every
    /// [`AudioRoute::fence_pending_frames`] call. Lets the supervisor
    /// (PR 4) and the unit tests assert the fence actually ran without
    /// having to peek at the pipeline receiver. Codex P2 #415
    /// audio_route.rs:368 (round 7-A).
    fences_run: u64,
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
            fences_run: 0,
        }
    }

    /// Test-only constructor that omits the pipeline. Tests drive
    /// `on_event` directly, so they do not need a real cpal stream.
    #[cfg(test)]
    pub(crate) fn for_test(session: DictateSession<T, I>, config: RouteConfig) -> Self {
        Self {
            pipeline: None,
            session,
            config,
            buffered_samples: 0,
            cap_tripped: false,
            fences_run: 0,
        }
    }

    /// Read-only access to the wrapped session. Lets the supervisor
    /// (PR 4) inspect state / epoch without re-borrowing the route.
    pub fn session(&self) -> &DictateSession<T, I> {
        &self.session
    }

    /// Current recording epoch (delegates to [`DictateSession::epoch`]).
    pub fn epoch(&self) -> u64 {
        self.session.epoch()
    }

    /// Current per-recording buffered-sample count.
    pub fn buffered_samples(&self) -> usize {
        self.buffered_samples
    }

    /// True once the in-flight recording tripped the max-record cap.
    pub fn cap_tripped(&self) -> bool {
        self.cap_tripped
    }

    /// Number of [`Self::fence_pending_frames`] calls observed so far.
    /// Tests + telemetry use this to confirm the supervisor actually
    /// fenced before the next press. Codex P2 #415 audio_route.rs:368
    /// (round 7-A).
    pub fn fences_run(&self) -> u64 {
        self.fences_run
    }

    /// Stop the underlying pipeline (no-op when running in test mode).
    pub fn stop_pipeline(&mut self) {
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.stop();
        }
    }

    /// Open a fresh utterance. Delegates to [`DictateSession::start`];
    /// on a successful start, re-reads the live-reloaded env knobs
    /// ([`MAX_RECORD_ENV`] + [`MIN_RECORD_ENV`]) so a Settings save
    /// between PTT presses takes effect on the next recording without
    /// rebuilding the route -- both settings are `live: true` in
    /// `src/python/whisper_dictate/settings_schema.json`, and the
    /// Python capture callbacks re-read them per recording. The min-
    /// record floor is mirrored into the session via
    /// [`DictateSession::update_min_record_seconds`] so
    /// [`DictateSession::stop_and_transcribe`] sees the new value.
    /// Codex P2 #415 audio_route.rs:250 + audio_route.rs:293.
    ///
    /// # Order matters: refresh AFTER the start succeeds
    ///
    /// Earlier round-7 versions of this method refreshed `self.config`
    /// BEFORE calling `session.start()`. That meant a duplicate-press /
    /// key-repeat that hit [`SessionError::AlreadyActive`] would still
    /// have mutated the in-flight recording cap mid-utterance even
    /// though no new recording began. We now stamp the new config only
    /// on the success path so the documented between-recordings
    /// reload contract is preserved. Codex P2 #415 audio_route.rs:293
    /// (round 7-C).
    ///
    /// Returns the new recording epoch so the caller can stash it for
    /// a later [`DictateSession::cancel`].
    pub fn start_recording<W: Write>(&mut self, writer: &mut W) -> Result<u64, RouteError> {
        let id = self.session.start(writer)?;
        // Success path only -- a refused start (AlreadyActive, writer
        // I/O failure) MUST NOT mutate the in-flight recording cap
        // or floor. Codex P2 #415 audio_route.rs:293 (round 7-C).
        let cfg = RouteConfig::from_env();
        // Mirror min-record floor into the session so
        // stop_and_transcribe skip helper sees the live-reloaded
        // value. Codex P2 #415 audio_route.rs:250 (round 7-D).
        self.session
            .update_min_record_seconds(cfg.min_record_seconds);
        self.config = cfg;
        self.buffered_samples = 0;
        self.cap_tripped = false;
        Ok(id)
    }

    /// Close the in-flight utterance. Delegates to
    /// [`DictateSession::stop_and_transcribe`].
    pub fn stop_recording<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, RouteError> {
        let outcome = self.session.stop_and_transcribe(writer)?;
        self.buffered_samples = 0;
        Ok(outcome)
    }

    /// Cancel the in-flight recording if `requested_epoch` matches the
    /// session current epoch. Delegates to [`DictateSession::cancel`].
    /// Codex P2 #415 audio_route.rs:251.
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
        // additionally erase the NEW recording sample count even
        // though the session correctly preserved its buffer.
        let active_before = self.session.epoch();
        self.session.cancel(requested_epoch, writer)?;
        if requested_epoch == active_before {
            self.buffered_samples = 0;
        }
        Ok(())
    }

    /// Drain stale [`PipelineEvent`]s the supervisor has queued in the
    /// pipeline receiver but not yet routed through [`Self::on_event`].
    /// Called by the supervisor BETWEEN [`Self::stop_recording`] and
    /// the next [`Self::start_recording`] to prevent stale frames from
    /// the previous press leaking into the new recording. Codex P2
    /// #415 audio_route.rs:368 (round 7-A).
    ///
    /// `PipelineEvent::Frame` carries no recording id and the
    /// idle-frame drop in [`Self::handle_frame`] only protects frames
    /// processed BEFORE the start handshake; frames queued in the
    /// channel before `start_recording()` but processed after it would
    /// otherwise land in the new utterance.
    ///
    /// The route does not own the channel, so the caller passes a
    /// drain callback that pops one buffered event per call (typically
    /// a closure wrapping `rx.try_recv().ok()`) and returns `None` when
    /// the channel has drained. The route discards every event the
    /// callback yields -- speech markers / device errors that landed in
    /// the fence window are no longer actionable.
    ///
    /// Returns the number of events drained so callers can log /
    /// telemeter how stale the receiver was. [`Self::fences_run`] is
    /// also bumped so tests + the supervisor can confirm the fence
    /// actually ran without inspecting the channel.
    pub fn fence_pending_frames<F>(&mut self, mut try_drain: F) -> usize
    where
        F: FnMut() -> Option<PipelineEvent>,
    {
        let mut drained: usize = 0;
        while try_drain().is_some() {
            drained = drained.saturating_add(1);
        }
        self.fences_run = self.fences_run.wrapping_add(1);
        drained
    }

    /// Drive a single [`PipelineEvent`] into the session.
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
                // epoch guard -- a stale Cancelled queued from a prior
                // recording could discard the new utterance. Python
                // Phase-1 rust-stdin handler also ignores Cancelled
                // (vp_capture_rust_stdin.py:228-232); the PTT-release
                // path is the authoritative cancel trigger. PR 4/5
                // will revisit when the Rust VAD drives commits.
                // Codex P2 #415 audio_route.rs:300.
                Ok(None)
            }
            PipelineEvent::DeviceError(msg) => {
                events::emit_device_error(&msg, writer);
                // Tear down the cpal worker + stream BEFORE returning
                // the device error. Codex P2 #415 audio_route.rs:327.
                self.stop_pipeline();
                Err(RouteError::Device(msg))
            }
        }
    }

    /// Speech-marker gate: only surface SpeechStart/SpeechEnd when a
    /// recording is in flight. Codex P2 #415 audio_route.rs:290.
    fn speech_marker_if_recording(&self, marker: SpeechMarker) -> Option<SpeechMarker> {
        if matches!(self.session.state(), SessionState::Recording { .. }) {
            Some(marker)
        } else {
            None
        }
    }

    /// The `Frame` branch of [`Self::on_event`].
    fn handle_frame<W: Write>(&mut self, frame: &[f32], writer: &mut W) -> Result<(), RouteError> {
        // Idle-frame drop. Mirrors vp_capture_rust_stdin.py:192-193.
        if !matches!(self.session.state(), SessionState::Recording { .. }) {
            return Ok(());
        }
        // Max-record cap. Mirrors vp_capture_rust_stdin.py:200-224.
        if let Some(cap) = self.config.max_record_seconds {
            let buffered_with_frame = self.buffered_samples.saturating_add(frame.len());
            let buffered_s = buffered_with_frame as f64 / f64::from(SR);
            if buffered_s > cap {
                if !self.cap_tripped {
                    self.cap_tripped = true;
                    events::emit_capped_status(buffered_s, writer)?;
                }
                return Ok(());
            }
        }
        self.session.push_frame(frame);
        self.buffered_samples = self.buffered_samples.saturating_add(frame.len());
        Ok(())
    }
}
