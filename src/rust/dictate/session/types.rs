//! Public types + trait boundaries for [`super::DictateSession`].
//!
//! Split out of `session/mod.rs` to keep that file focused on the
//! state-machine itself (start / push_frame / stop_and_transcribe /
//! cancel) and the wire-format emitter. All items here are re-exported
//! through `crate::dictate::session`.

use std::io;

/// Sample rate (Hz) the Whisper model consumes. Mirrors `SR` in
/// `vp_dictate.py`; pinned because the skip-gate and any future
/// duration-from-samples conversions assume this rate.
pub const SR: u32 = 16_000;

/// One transcription pass produced by a [`TranscribeBackend`].
///
/// Carries enough of the field set `vp_dictate.py::_transcription_event_fields`
/// reads to let `stop_and_transcribe` assemble a utterance event without
/// the backend knowing about the event schema. Numeric fields default to
/// zero so a minimal test backend can `..Default::default()` everything
/// it doesn't care about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscribeResult {
    /// The model's decoded text after the backend's own gates (Python's
    /// `result.text`). Empty string means the gate rejected the clip;
    /// the session treats that as the `no_speech` no-text path.
    pub text: String,
    /// True when the backend's `is_hallucination` filter flagged the
    /// text (Python's `is_hallucination(result.text)` branch in
    /// `_transcribe_pcm`). The session drops the utterance and emits a
    /// `no_text` event with `reason="no_speech"` — matching Python.
    pub is_hallucination: bool,
    /// Total compute time for this transcription pass, in milliseconds.
    /// Surfaced on the utterance event for the latency telemetry.
    pub latency_ms: u64,
    /// Detected audio duration in seconds (Python's `result.duration_s`).
    pub duration_s: f64,
    /// Detected language code (Python's `result.language`); empty for
    /// auto-detect.
    pub language: String,
    /// Python's `result.gate` -- the speech-gate verdict the backend
    /// returned, in whatever shape the gate produced (production
    /// gates return messages like `"input too quiet: -42 dBFS"` /
    /// `"no speech contrast: ..."`). The session passes this through
    /// `normalize_gate_reason` to translate the free-form text into one
    /// of `"too_quiet"` / `"no_speech"` / `"empty"` before emitting,
    /// matching the Python mapper. None when the backend produced
    /// usable text (the gate is irrelevant then).
    pub gate: Option<String>,
}

/// Errors a [`TranscribeBackend::transcribe`] call can surface. The
/// session translates each into a no-text event with the matching
/// Python `_transcribe_pcm` reason token.
#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    /// Model invocation itself failed (Python's `except Exception` in
    /// `_transcribe_pcm`; emitted as `reason="no_speech"`).
    #[error("transcribe backend error: {0}")]
    Backend(String),
}

/// Backend boundary for transcription. The production impl in PR 5 will
/// wrap `whisper-rs`; the test impl in `session/tests_support.rs` returns
/// canned results.
pub trait TranscribeBackend {
    /// Run inference on a mono PCM buffer at `sample_rate` Hz. The
    /// session always feeds 16 kHz mono (post-resample, post-channel-
    /// select) — `sample_rate` is passed explicitly so a future backend
    /// can validate / log the rate instead of trusting the constant.
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError>;
}

/// Errors an [`InjectBackend::inject`] call can surface.
#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    /// Generic injection failure (Python wraps the OS error and logs;
    /// the session does the same — it does not retry, matching
    /// `vp_dictate.py::_inject`).
    #[error("inject backend error: {0}")]
    Backend(String),
}

/// Backend boundary for text injection. The production impl in PR 4
/// will wrap `enigo` / `ydotool` / `xdotool`; the test impl in
/// `session/tests_support.rs` captures the text into a `Vec<String>` so
/// tests can assert exactly what would have been injected.
pub trait InjectBackend {
    /// Inject `text` into the focused window. The session calls this
    /// once per successful utterance, after post-processing has run.
    fn inject(&self, text: &str) -> Result<(), InjectError>;
}

/// Optional boundary for the LLM post-processing pass that runs AFTER
/// transcription and BEFORE the format-command layer + injection,
/// mirroring the `postprocess -> format -> inject` order in
/// `vp_dictate.py`.
///
/// Unlike [`TranscribeBackend`] / [`InjectBackend`] this seam is
/// OPTIONAL: a session with no post-processor configured
/// ([`super::DictateSession::post_process`] is `None`) skips the pass
/// entirely and does not emit the `post-processing` status, so the
/// default behaviour is byte-identical to a session that never knew
/// about post-processing. That is why it is a boxed `dyn` field with a
/// `None` default rather than a third generic type parameter on
/// [`super::DictateSession`] -- the pass runs at most once per
/// utterance, so the vtable indirection is irrelevant and the alternative
/// (threading a `P` through the coordinator sink, audio route, and every
/// test) is disproportionate churn.
///
/// The production impl wraps [`crate::postprocess::postprocess_text`],
/// which ALWAYS falls back to the input text on any provider / transport
/// error. So an implementation MUST NOT lose the user's dictation:
/// returning the input unchanged (via [`PostProcessOutcome`]) is the
/// correct behaviour when the rewrite is unavailable or empty.
pub trait PostProcessBackend {
    /// Rewrite `text` (cleanup / reformat) and report the pass metadata.
    /// The returned [`PostProcessOutcome::text`] must never be empty for
    /// non-empty input (fall back to the input instead).
    fn post_process(&self, text: &str) -> PostProcessOutcome;
}

/// Result of a [`PostProcessBackend`] pass: the (possibly rewritten) text
/// plus the metadata the session mirrors onto the `utterance` event as the
/// `post_*` fields Python emits (`vp_dictate.py:469-475`), consumed by
/// `ui/log_render.rs::post_processing_summary` + `telemetry.rs`. Kept as a
/// neutral struct in the session layer so `dictate` does not depend on
/// `crate::postprocess`; the production backend maps its
/// `PostprocessResult` onto these fields.
#[derive(Debug, Clone)]
pub struct PostProcessOutcome {
    /// Final text to inject (rewritten, or the input on fallback).
    pub text: String,
    /// `post_processor`: the provider that ran (`ollama` / `openai` / ...).
    pub processor: String,
    /// `post_mode`: the rewrite style (`clean` / `email` / ...).
    pub mode: String,
    /// `post_model`: the text model used.
    pub model: String,
    /// `post_latency_ms`: wall-clock time the provider call took.
    pub latency_ms: u64,
    /// `post_changed`: whether the rewrite differed from the input.
    pub changed: bool,
    /// `post_fallback`: whether the pass fell back to the input text.
    pub fallback: bool,
    /// `post_error`: provider/transport error message; empty when none
    /// (emitted as `null`/absent, matching Python's `error or None`).
    pub error: String,
    /// `post_redacted`: whether cloud-safe redaction replaced any terms
    /// before the provider call.
    pub redacted: bool,
    /// `post_redactions`: the public-safe redaction summary (placeholder /
    /// kind / char-count only, never the original values), mirroring
    /// Python's `post_result.redactions or []`.
    pub redactions: Vec<PostRedaction>,
}

/// One entry of [`PostProcessOutcome::redactions`] -- the public-safe
/// summary of a single redaction (`ui`/telemetry never see the original
/// value). Mirrors `crate::postprocess::RedactionSummary` /
/// Python's `RedactionResult.public_summary()` shape.
#[derive(Debug, Clone)]
pub struct PostRedaction {
    /// Placeholder token that replaced the sensitive value (e.g. `[[WD_1]]`).
    pub placeholder: String,
    /// Redaction kind (`email`, `phone`, `term`, ...).
    pub kind: String,
    /// Character length of the original value (length only, never the text).
    pub chars: usize,
}

/// Per-session configuration that mirrors the subset of `Dictate`
/// fields the per-utterance state machine actually reads. Loaded once
/// at session construction; live-reload is the supervisor's job (PR 5).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Hard floor on the captured-clip duration; clips below this are
    /// dropped with `reason="too_short"`. Mirrors Python's
    /// `min_record_seconds` setting. Clamped to
    /// [`crate::dictate::skip::MIN_RECORD_FLOOR_S`] inside the skip
    /// helper, so a misconfigured 0 still gets the 0.3 s misfire
    /// protection.
    pub min_record_seconds: f64,
    /// Capture-backend label surfaced on every status event. Mirrors
    /// `Dictate._capture_backend` (e.g. `"sounddevice"` / `"arecord"` /
    /// `"rust-stdin"`). PR 3 will populate this from the audio router;
    /// for tests / construction it is a free-form string.
    pub capture_backend: String,
    /// Active input-device label surfaced on every status event.
    /// Mirrors `Dictate._audio_input_device`.
    pub audio_device: String,
    /// Number of capture channels surfaced on every status event.
    /// Mirrors `Dictate._capture_channels`.
    pub capture_channels: u32,
    /// Spoken formatting-command set applied to the final transcript
    /// just before injection, mirroring Python's `format_commands`
    /// setting (`VOICEPI_FORMAT_COMMANDS`: `off` / `en` / `da` /
    /// `both`). Passed straight to
    /// [`crate::formatting::apply_format_commands`], whose
    /// `normalize_command_set` treats `None`, `Some("off")`, and any
    /// unknown-but-falsy value as a passthrough -- so a default-config
    /// session injects the raw transcript exactly as before this field
    /// existed. Stamped once at construction like `min_record_seconds`;
    /// live re-read is deferred to the same future PR that wires the
    /// audio route's per-`start_recording` env refresh.
    pub format_command_set: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            min_record_seconds: 0.5,
            capture_backend: String::new(),
            audio_device: String::new(),
            capture_channels: 1,
            format_command_set: None,
        }
    }
}

/// State-machine phases that mirror the observable transitions in
/// `vp_dictate.py`. `id` is the per-recording epoch — see
/// [`super::DictateSession::start`] / [`super::DictateSession::cancel`]
/// for the chord-race rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionState {
    /// Idle between utterances. Default constructor lands here.
    #[default]
    Idle,
    /// `start()` invoked, capture handshake not yet observable.
    /// `vp_dictate.py` emits a `status=opening` event in this window;
    /// the session does the same.
    Opening {
        /// Recording epoch this Opening corresponds to.
        id: u64,
    },
    /// Capture is live; frames passed to `push_frame()` are buffered.
    /// `vp_dictate.py` emits a `status=recording` event on entry.
    Recording {
        /// Recording epoch this Recording corresponds to.
        id: u64,
    },
    /// `stop_and_transcribe()` is running. The session never observably
    /// rests here in this PR (transcription is synchronous on the
    /// session thread), but the variant is reserved so PR 3/4 can move
    /// transcription to a background thread without an API change.
    Transcribing {
        /// Recording epoch this Transcribing corresponds to.
        id: u64,
    },
}

/// Errors a state-machine transition can refuse with.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    /// `start()` invoked while already in Opening/Recording/Transcribing.
    /// Mirrors `vp_dictate.py::_start`'s early-return on `self.recording`
    /// (no event, no state change).
    #[error("session is already active (state={state:?})")]
    AlreadyActive {
        /// State the session was in when the duplicate `start()` arrived.
        state: SessionState,
    },
    /// An I/O write to the event-line writer failed.
    #[error("event writer I/O error: {0}")]
    Io(String),
}

impl From<io::Error> for SessionError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

/// Why the session resolved a `stop_and_transcribe()` call the way it
/// did. Surfaced to callers (the supervisor in PR 5) so they can log /
/// drive UI without re-parsing the worker-event stream.
#[derive(Debug, Clone, PartialEq)]
pub enum UtteranceOutcome {
    /// `stop_and_transcribe()` ran while the session was idle (no
    /// recording in flight) — a no-op. Mirrors `vp_dictate.py`'s
    /// `if not self.recording: return` guard.
    NotRecording,
    /// A pending cancel (matching epoch) consumed the recording; the
    /// audio buffer was dropped, no transcription ran.
    Cancelled,
    /// The captured buffer was empty (no frames produced). Emits
    /// `no_text` with `reason="no_audio"`.
    NoAudio,
    /// The captured buffer was below the min-duration floor. Emits
    /// `no_text` with `reason="too_short"`.
    Skipped {
        /// Skip-reason token surfaced on the worker event (currently
        /// always `"too_short"`; widened if more skip categories land).
        reason: &'static str,
    },
    /// Transcription ran but produced no usable text (empty result,
    /// hallucination, too-quiet gate, …). Emits `no_text` with the
    /// matching reason token.
    NoText {
        /// Reason token surfaced on the worker event — `"no_speech"`,
        /// `"empty"`, `"too_quiet"`. Mirrors Python's `_transcribe_pcm`
        /// return values.
        reason: &'static str,
    },
    /// Transcription succeeded and the text was injected. The session
    /// hands the final text + the transcribe result back so the caller
    /// can build downstream events / telemetry without re-running the
    /// model.
    Injected {
        /// Text passed to `InjectBackend::inject`.
        text: String,
        /// Backend's raw result (latency, language, …).
        result: TranscribeResult,
    },
}
