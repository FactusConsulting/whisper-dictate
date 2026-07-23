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
    InjectBackend, InjectError, PostProcessBackend, PostProcessOutcome, PostRedaction,
    SessionConfig, SessionError, SessionState, TranscribeBackend, TranscribeError,
    TranscribeResult, UtteranceOutcome, SR,
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
    /// Optional LLM post-processing pass applied to the final transcript
    /// BEFORE the format-command layer and injection (Python's
    /// `postprocess -> format -> inject` order). `None` -- the default --
    /// skips the pass entirely and suppresses the `post-processing`
    /// status, so a session built with [`Self::new`] behaves exactly as
    /// before this seam existed. Set via [`Self::with_post_process`].
    post_process: Option<Box<dyn PostProcessBackend + Send>>,
    /// Optional dictionary whose deterministic replacement table rewrites the
    /// transcript FIRST -- before post-processing, formatting and injection --
    /// mirroring Python's `_dictionary_runtime(raw_text)` step in
    /// `vp_transcribe._transcribe_detail` (replacements are applied to the
    /// decoded text before it leaves the transcribe path). `None` -- the
    /// default -- applies no replacements, so a session built with
    /// [`Self::new`] behaves exactly as before this seam existed. Set via
    /// [`Self::with_dictionary`].
    dictionary: Option<crate::dictionary::Dictionary>,
}

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Build a fresh session. The session starts in
    /// [`SessionState::Idle`] with an empty buffer and `epoch == 0`, and
    /// with no post-processor (use [`Self::with_post_process`] to attach
    /// one).
    pub fn new(transcribe: T, inject: I, config: SessionConfig) -> Self {
        Self {
            state: SessionState::Idle,
            frame_buf: Vec::new(),
            epoch: 0,
            config,
            transcribe,
            inject,
            post_process: None,
            dictionary: None,
        }
    }

    /// Attach an LLM post-processing backend, returning the session so
    /// callers can chain it after [`Self::new`]. When set, a successful
    /// utterance runs `backend.post_process(text)` (emitting a
    /// `post-processing` status) before the format-command layer and
    /// injection. Passing this is opt-in: the production wiring only
    /// attaches a backend when the operator configured a post-processor
    /// (`VOICEPI_POST_PROCESSOR` != `none`).
    pub fn with_post_process(mut self, backend: Box<dyn PostProcessBackend + Send>) -> Self {
        self.post_process = Some(backend);
        self
    }

    /// Attach a dictionary whose replacement table rewrites the transcript
    /// BEFORE post-processing, formatting and injection -- mirroring Python's
    /// `_dictionary_runtime(raw_text)` step in `vp_transcribe._transcribe_detail`
    /// (replacements are applied to the decoded text before it leaves the
    /// transcribe path). Passing this is opt-in: the production wiring only
    /// attaches a dictionary when the configured one actually has replacements,
    /// so a session without one is byte-identical to before this seam existed.
    /// (Term-based prompt biasing -- the other half of dictionary support -- is
    /// applied at backend-config construction, not here.)
    pub fn with_dictionary(mut self, dictionary: crate::dictionary::Dictionary) -> Self {
        self.dictionary = Some(dictionary);
        self
    }

    /// Attach a [`crate::dictionary::SessionDictionary`]'s replacement table
    /// only when it actually carries replacements, mirroring the guard every
    /// production call site (`simulate-session`, `make_real_session`) would
    /// otherwise repeat inline. An empty / disabled dictionary is a no-op, so
    /// the session stays byte-identical to one built without a dictionary. The
    /// term-based prompt biasing (the other half of dictionary support) is
    /// folded into the backend config beforehand via
    /// [`crate::dictionary::SessionDictionary::fold_into_prompt`]; this seam
    /// owns only the replacement table.
    pub fn with_optional_dictionary(
        self,
        dictionary: crate::dictionary::SessionDictionary,
    ) -> Self {
        if dictionary.has_replacements() {
            self.with_dictionary(dictionary.dictionary)
        } else {
            self
        }
    }

    /// Apply the attached dictionary's replacement table to `text`, returning
    /// the rewritten string and the per-replacement change records (for the
    /// utterance event's `dictionary_replacements` field). A `None` dictionary,
    /// an empty replacement table, or empty text is a passthrough (no changes);
    /// a replacement regex error keeps the original text (a replacement failure
    /// must never drop a dictation). Pure so the wiring is unit-testable
    /// without a real transcribe backend.
    fn apply_dictionary(&self, text: &str) -> (String, Vec<crate::dictionary::ReplacementChange>) {
        match &self.dictionary {
            Some(dict) if !text.is_empty() => dict
                .apply_replacements(text)
                .unwrap_or_else(|_| (text.to_owned(), Vec::new())),
            _ => (text.to_owned(), Vec::new()),
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

        // Transcribe, then run the dictionary replacement table FIRST --
        // BEFORE the empty / hallucination classification -- matching Python,
        // where `_dictionary_runtime(raw_text)` in `_transcribe_detail`
        // rewrites the text before `_transcribe_pcm` performs its
        // empty/hallucination checks. A replacement whose SOURCE is a blacklist
        // phrase (e.g. mapping "tak" -> "tak.") is therefore applied and the
        // CORRECTED text is (re)classified.
        let mut result = match self.transcribe.transcribe(buf, SR) {
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
                return Ok(UtteranceOutcome::NoText {
                    reason: "no_speech",
                });
            }
            Ok(result) => result,
        };

        let (dictated, replacements) = self.apply_dictionary(&result.text);
        if dictated != result.text {
            // The dictionary rewrote the text; re-classify the corrected text
            // so a replacement can turn a blacklist phrase into normal
            // dictation (or vice versa). When nothing changed we keep the
            // backend's own `is_hallucination` verdict untouched.
            result.is_hallucination = crate::dictate::backends::is_hallucination(dictated.trim());
        }
        result.text = dictated;

        if result.text.is_empty() {
            // Python distinguishes `too_quiet`, `no_speech`, `empty` from
            // `result.gate` so the matching UI card fires. The production gate
            // returns free-form text (e.g. "input too quiet: -42 dBFS"), so
            // route it through `normalize_gate_reason` to land on one of the
            // three reason tokens. Codex P2 #413 mod.rs:263 + mod.rs:284.
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
            return Ok(UtteranceOutcome::NoText { reason });
        }

        if result.is_hallucination {
            wire::emit_status(
                writer,
                "no_text",
                &[
                    ("reason", Value::from("no_speech")),
                    ("recording_s", recording_s.clone()),
                ],
            )?;
            return Ok(UtteranceOutcome::NoText {
                reason: "no_speech",
            });
        }

        // Text pipeline between transcription and injection, mirroring the
        // `dictionary -> postprocess -> format -> inject` order in
        // `vp_dictate.py` / `vp_transcribe.py` (the dictionary step ran above):
        //
        // 1. LLM post-processing (optional). When a `PostProcessBackend` is
        //    attached the session emits a `post-processing` status and runs the
        //    rewrite; the production impl falls back to the input text on any
        //    provider error, so the user's dictation is never lost. `None` (the
        //    default) skips this pass AND its status entirely.
        // 2. Deterministic spoken formatting commands (`new line` -> "\n",
        //    `comma` -> ",", ...) -- a pure string transform; a `None` / `off`
        //    command set is a passthrough.
        //
        // The emitted `utterance` event carries the fully pipelined text (what
        // was actually injected) plus the `post_*` and `dictionary_replacements`
        // metadata, matching Python (`vp_dictate.py:469-475`) so
        // `ui/log_render.rs` + telemetry see what the pipeline did.
        let post = if let Some(backend) = self.post_process.as_ref() {
            wire::emit_status(writer, "post-processing", &self.capture_extras())?;
            Some(backend.post_process(&result.text))
        } else {
            None
        };
        let post_processed = post
            .as_ref()
            .map(|o| o.text.clone())
            .unwrap_or_else(|| result.text.clone());
        let text = crate::formatting::apply_format_commands(
            &post_processed,
            self.config.format_command_set.as_deref(),
        )
        .text;
        if let Err(err) = self.inject.inject(&text) {
            // Python logs and continues — the utterance event still fires with
            // the text we attempted to inject. Surface the failure on the
            // utterance event so the supervisor can decide whether to retry.
            wire::emit_utterance(
                writer,
                &text,
                &result,
                recording_s.clone(),
                Some(err.to_string()),
                post.as_ref(),
                &replacements,
            )?;
            return Ok(UtteranceOutcome::Injected { text, result });
        }
        wire::emit_utterance(
            writer,
            &text,
            &result,
            recording_s.clone(),
            None,
            post.as_ref(),
            &replacements,
        )?;
        Ok(UtteranceOutcome::Injected { text, result })
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
