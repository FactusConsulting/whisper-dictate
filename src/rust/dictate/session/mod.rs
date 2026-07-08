//! Pure-logic per-utterance state machine for the live PTT dictation
//! loop. Mirrors `src/python/whisper_dictate/vp_dictate.py::Dictate`'s
//! per-utterance lifecycle (start → push frames → stop/transcribe →
//! inject, with cancel) but with NO audio capture, NO model loading and
//! NO real injection: every side-effecting boundary goes through a
//! trait-bound mock so unit tests run without cpal / whisper / enigo.
//!
//! Wave 5 PR 2 of issue #348. The audio-route (PR 3), hotkey wiring
//! (PR 4) and full Rust supervisor (PR 5+) are what consume this — there
//! is no production caller in this PR yet. Adding the state machine in
//! isolation lets the per-utterance transition logic be unit-tested
//! end-to-end (the six characterisation tests ported from
//! `src/python/tests/test_dictate_loop.py`) before the I/O layer lands.
//!
//! # Why a trait-bound design
//!
//! `vp_dictate.py` weaves capture / transcribe / inject side-effects
//! into the same per-utterance method that owns the state machine, which
//! is why `test_dictate_loop.py` has to build a `Dictate` via
//! `object.__new__` and monkey-patch six boundary functions to test the
//! orchestration. Splitting capture / transcribe / inject out as traits
//! up-front gives us the same testability without the monkey-patching
//! gymnastics, and is the shape PR 3/4 already need anyway because cpal
//! lives in a `cfg(feature = "audio-in-rust")` module.
//!
//! # Module layout
//!
//! - [`types`] — public trait boundaries + result / state / error / config
//!   types. Re-exported through this module.
//! - [`wire`] — the narrow `[worker-event] {…}\n` line emitter the
//!   session uses for status / utterance events. Will be swapped for
//!   the richer `crate::dictate::events` emitter from PR 1 (#412) by
//!   PR 3, once both PRs are in `main`.
//! - [`tests_support`] — `cfg(test)` test backends + helpers shared
//!   across the test files.
//! - [`tests_ported`] — the six characterisation tests ported from
//!   `src/python/tests/test_dictate_loop.py`.
//! - [`tests_transitions`] — supplementary state-transition invariants.

use std::io::Write;

use serde_json::{json, Value};

pub mod types;
mod wire;

#[cfg(test)]
mod tests_ported;
#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_transitions;

pub use types::{
    InjectBackend, InjectError, SessionConfig, SessionError, SessionState, TranscribeBackend,
    TranscribeError, TranscribeResult, UtteranceOutcome, SR,
};

/// Translate the backend's free-form gate text (as `result.gate` carries
/// it -- e.g. `"input too quiet: -42 dBFS"`, `"no speech contrast: 0.02"`)
/// into one of the three reason tokens the worker-event consumers / UI
/// cards switch on: `"too_quiet"`, `"no_speech"`, `"empty"`. Mirrors the
/// Python mapper in `vp_transcribe.py` (substring-based, ASCII-cased).
/// Codex P2 #413 mod.rs:284 (round 2 follow-up to the `gate` field
/// landed in round 1).
pub(crate) fn normalize_gate_reason(gate: &str) -> &'static str {
    let lowered = gate.to_ascii_lowercase();
    if lowered.contains("too quiet") {
        return "too_quiet";
    }
    if lowered.contains("no speech") {
        return "no_speech";
    }
    "empty"
}

/// Per-utterance state machine. Owns the capture buffer and the
/// transcribe / inject backends; emits status events through a
/// caller-supplied writer.
///
/// See the module docs for the design rationale. See `tests_ported.rs`
/// for the six characterisation tests ported from
/// `src/python/tests/test_dictate_loop.py` and `tests_transitions.rs`
/// for the supplementary state-transition invariants.
pub struct DictateSession<T: TranscribeBackend, I: InjectBackend> {
    state: SessionState,
    /// Captured PCM at the model's sample rate (16 kHz mono). In this
    /// PR `push_frame` already-resampled samples; PR 3 owns the
    /// channel-select + resample at consumption.
    frame_buf: Vec<f32>,
    /// Monotonic recording generation. Bumped on every `start()` so
    /// the chord-race guard in `cancel()` can detect a stale request.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    epoch: u64,
    config: SessionConfig,
    transcribe: T,
    inject: I,
}

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Build a fresh session. The session starts in
    /// [`SessionState::Idle`] with an empty buffer and `epoch == 0`.
    pub fn new(transcribe: T, inject: I, config: SessionConfig) -> Self {
        Self {
            state: SessionState::Idle,
            frame_buf: Vec::new(),
            epoch: 0,
            config,
            transcribe,
            inject,
        }
    }

    /// Current state-machine phase. Exposed for tests and the
    /// supervisor's UI; the session itself is the source of truth.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Current recording epoch. Returned by [`Self::start`] and read by
    /// [`Self::cancel`] for the chord-race guard.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Re-set the per-session min-record floor in seconds. The
    /// `min_record_seconds` setting is `live: true` in
    /// `src/python/whisper_dictate/settings_schema.json`; the audio
    /// route calls this on every successful
    /// [`crate::dictate::audio_route::AudioRoute::start_recording`]
    /// (after re-reading [`crate::dictate::audio_route::MIN_RECORD_ENV`])
    /// so a Settings save between PTT presses takes effect on the next
    /// recording without rebuilding the session. The skip helper still
    /// clamps the effective floor up to
    /// [`crate::dictate::skip::MIN_RECORD_FLOOR_S`] (0.3 s) regardless,
    /// so a misconfigured 0 still surfaces the misfire protection.
    /// Codex P2 #415 audio_route.rs:250 (round 7-D).
    pub fn update_min_record_seconds(&mut self, seconds: f64) {
        self.config.min_record_seconds = seconds;
    }

    /// Read-only access to the transcribe backend. Tests use this to
    /// inspect what the session passed to the mock; production callers
    /// will rarely need it.
    pub fn transcribe_backend(&self) -> &T {
        &self.transcribe
    }

    /// Read-only access to the inject backend. Tests use this to assert
    /// what the session injected.
    pub fn inject_backend(&self) -> &I {
        &self.inject
    }

    /// Open a fresh utterance.
    ///
    /// Mirrors `vp_dictate.py::_start`:
    /// 1. early-return if a recording is already in flight (no events,
    ///    no state change — same guard as Python's `if self.recording`);
    /// 2. clear the frame buffer;
    /// 3. bump the recording epoch (the chord-race generation counter
    ///    — see `vp_dictate.py:140-147`);
    /// 4. emit `status=opening`;
    /// 5. transition to [`SessionState::Recording`] and emit
    ///    `status=recording` with capture backend / device / channels.
    ///
    /// Returns the new epoch so the caller (e.g. a chord-cancel
    /// dispatcher) can stamp the value and pass it back to
    /// [`Self::cancel`].
    pub fn start<W: Write>(&mut self, writer: &mut W) -> Result<u64, SessionError> {
        if !matches!(self.state, SessionState::Idle) {
            return Err(SessionError::AlreadyActive { state: self.state });
        }
        self.frame_buf.clear();
        self.epoch = self.epoch.wrapping_add(1);
        let id = self.epoch;
        self.state = SessionState::Opening { id };
        // If either emit fails, the caller never receives `id` and a
        // subsequent `start()` would refuse with `AlreadyActive` -- the
        // session would wedge unless something else reset it. Roll back
        // to Idle on the failure path so the next press recovers cleanly.
        // The epoch stays bumped (it's a monotonic counter; gaps are
        // harmless). Codex P2 #413 mod.rs:144.
        if let Err(e) = wire::emit_status(writer, "opening", &[]) {
            self.state = SessionState::Idle;
            return Err(e);
        }
        self.state = SessionState::Recording { id };
        if let Err(e) = wire::emit_status(writer, "recording", &self.capture_extras()) {
            self.state = SessionState::Idle;
            return Err(e);
        }
        Ok(id)
    }

    /// Append a chunk of post-resample, post-channel-select PCM to the
    /// capture buffer.
    ///
    /// Frames pushed while the session is not in [`SessionState::Recording`]
    /// are silently dropped — matching the Python capture mixin, which
    /// gates frame ingestion on `self.recording == True`. This makes the
    /// session safe to drive from a long-lived audio reader thread that
    /// outlives any single utterance.
    pub fn push_frame(&mut self, frame: &[f32]) {
        if matches!(self.state, SessionState::Recording { .. }) {
            self.frame_buf.extend_from_slice(frame);
        }
    }

    /// Close the recording, decide skip / hallucination / inject, and
    /// emit the matching status + utterance events.
    ///
    /// Mirrors `vp_dictate.py::_stop_and_transcribe`:
    /// * empty buffer → `status=no_text reason=no_audio`,
    ///   returns [`UtteranceOutcome::NoAudio`].
    /// * buffer below the min-duration floor →
    ///   `status=no_text reason=too_short`, returns
    ///   [`UtteranceOutcome::Skipped`].
    /// * backend error or empty / hallucinated text →
    ///   `status=no_text reason=…`, returns [`UtteranceOutcome::NoText`].
    /// * success → inject, emit `event=utterance`, return
    ///   [`UtteranceOutcome::Injected`].
    ///
    /// Always returns to [`SessionState::Idle`] before returning, even
    /// on error (matching Python's `finally:` that emits `status=ready`).
    pub fn stop_and_transcribe<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> Result<UtteranceOutcome, SessionError> {
        if !matches!(self.state, SessionState::Recording { .. }) {
            // Mirrors `if not self.recording: return` in Python. No
            // events, no state change.
            return Ok(UtteranceOutcome::NotRecording);
        }
        let id = match self.state {
            SessionState::Recording { id } => id,
            // Unreachable thanks to the matches! above, but pattern-
            // matching keeps the compiler honest if SessionState gains
            // a variant later.
            _ => unreachable!("guarded by matches! above"),
        };
        self.state = SessionState::Transcribing { id };

        // Drain the buffer up-front so any early-return path leaves the
        // session ready for the next press.
        let buf = std::mem::take(&mut self.frame_buf);

        let outcome = self.run_transcription(writer, &buf);
        // Always settle back to Idle + emit `status=ready`, matching
        // Python's `finally: _emit_worker_event(..., state="ready")`.
        self.state = SessionState::Idle;
        wire::emit_status(writer, "ready", &self.capture_extras())?;
        outcome
    }

    /// The post-`Transcribing` branch from `_stop_and_transcribe`.
    /// Split out so the `finally`-equivalent reset + `status=ready` at
    /// the bottom of `stop_and_transcribe` cannot drift out of sync
    /// with the early-return paths.
    fn run_transcription<W: Write>(
        &mut self,
        writer: &mut W,
        buf: &[f32],
    ) -> Result<UtteranceOutcome, SessionError> {
        // Status flip to `transcribing` lands FIRST -- Python's
        // `vp_dictate.py` emits this before the empty-frames guard, so
        // every utterance shows `recording -> transcribing -> ... -> ready`
        // even when no frames arrive (consumers like log_render.rs key on
        // the full sequence). Without this, the no-audio path would jump
        // straight from `recording` to `no_text` and drop the
        // `transcribing` UI card. Codex P2 #413 mod.rs:233 (round 2).
        wire::emit_status(writer, "transcribing", &self.capture_extras())?;

        // No frames ever pushed — Python's `if not self.frames:` branch.
        if buf.is_empty() {
            wire::emit_status(writer, "no_text", &[("reason", Value::from("no_audio"))])?;
            return Ok(UtteranceOutcome::NoAudio);
        }

        // `recording_s` is reported on every no-text branch (Python:
        // `recording_s=round(recording_s, 2)` on every `_emit_worker_event`
        // call from `_stop_and_transcribe`'s no-text paths) AND on the
        // successful utterance event. Computed once up-front so every
        // branch shares the same value. Codex P2 #413 mod.rs:254 +
        // wire.rs:61 (round 2).
        let recording_s = json!(wire::round2(buf.len() as f64 / SR as f64));

        // Min-duration gate. Delegates to the existing skip helper so
        // the threshold semantics (0.3 s floor, fractional comparison)
        // stay in lock-step with `Dictate._should_skip_pcm`.
        let skip = crate::dictate::skip::should_skip(buf.len(), self.config.min_record_seconds);
        if let Some(reason) = skip.reason() {
            wire::emit_status(
                writer,
                "no_text",
                &[
                    ("reason", Value::from(reason)),
                    ("recording_s", recording_s.clone()),
                ],
            )?;
            return Ok(UtteranceOutcome::Skipped { reason });
        }

        match self.transcribe.transcribe(buf, SR) {
            Err(err) => {
                // Python wraps the error and treats it as no_speech.
                wire::emit_status(
                    writer,
                    "no_text",
                    &[
                        ("reason", Value::from("no_speech")),
                        ("error", Value::from(err.to_string())),
                        ("recording_s", recording_s.clone()),
                    ],
                )?;
                Ok(UtteranceOutcome::NoText {
                    reason: "no_speech",
                })
            }
            Ok(result) if result.text.is_empty() => {
                // Python distinguishes `too_quiet`, `no_speech`, `empty`
                // from `result.gate` so the matching UI card fires. The
                // production gate returns free-form text (e.g. "input
                // too quiet: -42 dBFS"), so route it through
                // `normalize_gate_reason` to land on one of the three
                // reason tokens. Codex P2 #413 mod.rs:263 (round 1) +
                // mod.rs:284 (round 2 follow-up: gate text normalisation).
                let reason = result
                    .gate
                    .as_deref()
                    .map(normalize_gate_reason)
                    .unwrap_or("empty");
                wire::emit_status(
                    writer,
                    "no_text",
                    &[
                        ("reason", Value::from(reason)),
                        ("recording_s", recording_s.clone()),
                    ],
                )?;
                Ok(UtteranceOutcome::NoText { reason })
            }
            Ok(result) if result.is_hallucination => {
                wire::emit_status(
                    writer,
                    "no_text",
                    &[
                        ("reason", Value::from("no_speech")),
                        ("recording_s", recording_s.clone()),
                    ],
                )?;
                Ok(UtteranceOutcome::NoText {
                    reason: "no_speech",
                })
            }
            Ok(result) => {
                // Wave 5.5 gap #2 + #3: run post-processing then format
                // commands between transcribe and inject. Mirrors
                // `vp_dictate.py::_stop_and_transcribe`'s
                // `_postprocess_and_format(text)` call (see the Python
                // path around vp_dictate.py:810). Order is fixed:
                // transcribe -> postprocess -> format -> inject so a
                // format-command like "period" applied by the user
                // after an LLM cleanup still becomes a real `.`.
                let raw_text = result.text.clone();
                let (post_text, post_extras) = self.run_postprocess(writer, &raw_text)?;
                let (final_text, format_extras) = self.run_format_commands(&post_text);
                let extras = wire::UtteranceExtras {
                    raw_text: (final_text != raw_text).then(|| raw_text.clone()),
                    postprocess_error: post_extras.error,
                    post_provider: post_extras.provider,
                    post_mode: post_extras.mode,
                    post_latency_ms: post_extras.latency_ms,
                    post_changed: post_extras.changed,
                    post_fallback: post_extras.fallback,
                    format_enabled: format_extras.enabled,
                    format_command_set: format_extras.command_set,
                    format_changed: format_extras.changed,
                };
                if let Err(err) = self.inject.inject(&final_text) {
                    // Python logs and continues — the utterance event
                    // still fires with the text we attempted to inject.
                    // Surface the failure on the utterance event so the
                    // supervisor can decide whether to retry / show UI.
                    wire::emit_utterance(
                        writer,
                        &final_text,
                        &result,
                        recording_s.clone(),
                        Some(err.to_string()),
                        extras,
                    )?;
                    return Ok(UtteranceOutcome::Injected {
                        text: final_text,
                        result,
                    });
                }
                wire::emit_utterance(
                    writer,
                    &final_text,
                    &result,
                    recording_s.clone(),
                    None,
                    extras,
                )?;
                Ok(UtteranceOutcome::Injected {
                    text: final_text,
                    result,
                })
            }
        }
    }

    /// Run the post-processing pipeline on the transcribed text. When
    /// no post-processor settings are configured (the default), returns
    /// the input text unchanged with empty extras -- the wire event
    /// omits all `post_*` fields and stays byte-identical to the
    /// pre-Wave-5.5 shape.
    ///
    /// Emits a `status=post-processing` worker event before the call
    /// when the settings will actually invoke a provider (mirrors
    /// `vp_dictate.py:798-809` -- only fires the UI card when the
    /// processor is not `none` and the mode is not `raw`), so the live
    /// pipeline card lights up in sync with the Python path.
    ///
    /// On failure the post-processor itself falls back to the original
    /// text (see [`crate::postprocess::postprocess_text`]'s
    /// `PostprocessResult::fallback` field), so this helper NEVER
    /// aborts injection -- it surfaces the error via
    /// [`PostprocessRunExtras::error`] so the utterance event can
    /// report it.
    fn run_postprocess<W: Write>(
        &self,
        writer: &mut W,
        raw_text: &str,
    ) -> Result<(String, PostprocessRunExtras), SessionError> {
        let Some(settings) = &self.config.postprocess_settings else {
            return Ok((raw_text.to_owned(), PostprocessRunExtras::default()));
        };
        let mode_short = crate::postprocess::normalize_mode(&settings.mode);
        // Python fires the `post-processing` status card ONLY when a
        // provider is actually going to run. Mirror that so the UI
        // sequence stays identical.
        if settings.processor != "none" && mode_short != "raw" {
            wire::emit_status(writer, "post-processing", &self.capture_extras())?;
        }
        let result = crate::postprocess::postprocess_text(raw_text, settings);
        let extras = PostprocessRunExtras {
            provider: Some(result.provider.clone()),
            mode: Some(result.mode.clone()),
            latency_ms: Some(result.latency_ms),
            changed: Some(result.changed),
            fallback: Some(result.fallback),
            error: (!result.error.is_empty()).then(|| result.error.clone()),
        };
        Ok((result.text, extras))
    }

    /// Run format commands on the (possibly post-processed) text.
    /// Off by default via `SessionConfig::format_commands = "off"`.
    /// Mirrors `vp_dictate.py::_postprocess_and_format`'s call to
    /// `apply_format_commands(post_result.text)` -- the input is the
    /// POST-PROCESSED text, matching Python order.
    fn run_format_commands(&self, text: &str) -> (String, FormatRunExtras) {
        let cmd_set = self.config.format_commands.as_str();
        let result = crate::formatting::apply_format_commands(text, Some(cmd_set));
        let extras = FormatRunExtras {
            enabled: Some(result.enabled),
            command_set: Some(result.command_set.clone()),
            changed: Some(result.changed),
        };
        (result.text, extras)
    }

    /// Discard the in-flight recording if `requested_epoch` matches the
    /// current recording generation.
    ///
    /// This is the chord-cancel race guard. The chord-cancel callback in
    /// `vp_keys` runs on a daemon thread that may be delayed past a
    /// release + re-press; it captures the recording generation at
    /// chord-detection time and passes it back here. Without the epoch
    /// guard a stale cancel would silently discard the NEW recording.
    /// See `vp_dictate.py:140-147 + 665-684` for the exact race.
    ///
    /// On a matching epoch:
    /// * drops the buffered frames,
    /// * settles back to [`SessionState::Idle`],
    /// * emits `status=cancelled reason=chord` then `status=ready`,
    ///   matching Python.
    ///
    /// On a stale epoch (or while idle): no-op, no events, no state
    /// change.
    pub fn cancel<W: Write>(
        &mut self,
        requested_epoch: u64,
        writer: &mut W,
    ) -> Result<(), SessionError> {
        let active_id = match self.state {
            SessionState::Recording { id } | SessionState::Opening { id } => id,
            // Idle / Transcribing: nothing to cancel. Transcribing is
            // racy in Python too — the cancel arrives after capture has
            // already stopped, so the audio is already on its way to
            // the model; matching Python we no-op.
            _ => return Ok(()),
        };
        if requested_epoch != active_id {
            // Stale cancel — the NEW recording's epoch is `active_id`,
            // not `requested_epoch`. Must NOT discard. This is the
            // load-bearing race-correctness check.
            return Ok(());
        }
        self.frame_buf.clear();
        self.state = SessionState::Idle;
        wire::emit_status(writer, "cancelled", &[("reason", Value::from("chord"))])?;
        wire::emit_status(writer, "ready", &self.capture_extras())?;
        Ok(())
    }

    /// The capture-backend / audio-device / capture-channels extras
    /// every status event carries. Empty strings and zero values are
    /// dropped by [`wire::emit_status`], so an unconfigured session
    /// emits a clean minimal event.
    fn capture_extras(&self) -> [(&'static str, Value); 3] {
        [
            (
                "capture_backend",
                Value::from(self.config.capture_backend.clone()),
            ),
            (
                "audio_device",
                Value::from(self.config.audio_device.clone()),
            ),
            (
                "capture_channels",
                Value::from(self.config.capture_channels),
            ),
        ]
    }
}

/// Provenance extracted from one post-processing pass. Consumed by
/// [`DictateSession::stop_and_transcribe`] to build the utterance event's
/// `post_*` fields.
///
/// All fields are `Option` so a session with no post-processor
/// configured emits `Default::default()` extras — every field stays
/// `None`, which the wire emitter turns into an omitted field, and the
/// event is byte-identical to the pre-Wave-5.5 shape.
#[derive(Debug, Clone, Default)]
struct PostprocessRunExtras {
    provider: Option<String>,
    mode: Option<String>,
    latency_ms: Option<u64>,
    changed: Option<bool>,
    fallback: Option<bool>,
    /// Only populated when the post-processor set a non-empty `error`
    /// message. Surfaced on the utterance event via
    /// [`wire::UtteranceExtras::postprocess_error`] so the supervisor
    /// UI can drive a "post fallback" indicator without re-parsing
    /// logs. Python's `_postprocess_and_format` prints the same
    /// message to `[post]`; we surface it as structured wire data.
    error: Option<String>,
}

/// Provenance extracted from one format-commands pass. Mirrors
/// [`PostprocessRunExtras`]; empty defaults keep the utterance event
/// backwards-compatible.
#[derive(Debug, Clone, Default)]
struct FormatRunExtras {
    enabled: Option<bool>,
    command_set: Option<String>,
    changed: Option<bool>,
}
