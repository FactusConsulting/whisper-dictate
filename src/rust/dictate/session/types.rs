//! Public types + trait boundaries for [`super::DictateSession`].
//!
//! Split out of `session/mod.rs` to keep that file focused on the
//! state-machine itself (start / push_frame / stop_and_transcribe /
//! cancel) and the wire-format emitter. All items here are re-exported
//! through `crate::dictate::session`.

use std::collections::BTreeMap;
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
    /// Detected-language probability from the backend (Python's
    /// `result.language_probability` -- faster-whisper surfaces this on
    /// every pass). `0.0` when the backend does not surface a score
    /// (e.g. cloud STT, canned test fixtures). Emitted verbatim on the
    /// metrics utterance event so external tooling sees the same
    /// signal the Python engine writes today. Codex P1 #606 metrics-schema
    /// follow-up.
    pub language_probability: f64,
    /// Untouched decoded text as the backend produced it, BEFORE the
    /// per-utterance dictionary replacement pass rewrites `text`.
    /// Mirrors Python's `result.raw_text` in `vp_transcribe.TranscribeResult`
    /// (populated by `_transcribe_detail` before `_dictionary_runtime`).
    /// Emitted verbatim on the utterance event so metrics / history
    /// carry the pre-dictionary form for auditing. Empty when the
    /// backend does not surface a distinct raw copy; the session then
    /// falls back to the dictionary-rewritten text at event build time
    /// (mirrors Python's `result.raw_text or source_text`). Codex P1
    /// #606 metrics-schema follow-up.
    pub raw_text: String,
    /// Which transcription implementation ACTUALLY produced this result
    /// (`crate::dictate::provenance::STT_IMPL_*`: `"whisper.cpp"`,
    /// `"cloud-openai"`, `"cloud-groq"`, ...). Distinct from
    /// [`SessionConfig::stt_backend`], which is the *configured* backend
    /// name (`"whisper"` / `"openai"`) and so cannot tell a
    /// whisper.cpp run apart from a faster-whisper one. Emitted as the
    /// `stt_impl` field on the utterance record; empty on a
    /// default-constructed test result, which the wire emitter drops.
    pub stt_impl: String,
    /// Which compute path this pass actually ran on
    /// (`crate::whisper::accel::Accel::as_str`: `"vulkan"` / `"cuda"` /
    /// `"cpu"` / `"unknown"`). Resolved from what the backend REPORTED at
    /// transcription time -- for the local whisper.cpp path, from its own
    /// `whisper_backend_init_gpu` model-load log line -- NOT from the
    /// `device` setting, which is typically `auto` and says nothing about
    /// the outcome. Emitted as the `stt_accel` field; empty on a
    /// default-constructed test result.
    pub stt_accel: String,
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

    /// Apply per-utterance profile overrides to this backend (Python parity
    /// port of the settings hot-swap in `vp_dictate._apply_effective_config`).
    /// The session calls this from `apply_active_profile` before every
    /// utterance, passing the profile's raw `settings` map (empty when no
    /// profile matched, so the backend can reset its overrides between
    /// presses). Each backend picks the subset of keys it cares about (e.g.
    /// `initial_prompt`, `language`, `model`) and stores them behind interior
    /// mutability so a per-utterance `transcribe(&self, ...)` call sees the
    /// override. Default impl is a no-op so mock backends do not have to
    /// know about the profile system.
    fn apply_profile_overrides(&self, _settings: &BTreeMap<String, String>) {}
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

    /// Apply per-utterance profile overrides. See
    /// [`TranscribeBackend::apply_profile_overrides`] for the contract; the
    /// production impl reads the `inject_mode` key to switch between
    /// typing / paste / print for a single utterance without rebuilding
    /// the backend.
    fn apply_profile_overrides(&self, _settings: &BTreeMap<String, String>) {}
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

    /// True when this backend will actually rewrite the input this utterance.
    /// The session calls this AFTER [`Self::apply_profile_overrides`] so a
    /// profile that flips `post_processor=ollama` on a session initially
    /// constructed with a `none` processor still runs. When it returns
    /// `false` the session skips the `post-processing` status emission and
    /// the [`Self::post_process`] call entirely, matching Python's gate on
    /// `processor != "none" && mode != "raw"`. Default `true` keeps the
    /// existing behaviour for backends that never disable themselves (e.g.
    /// the test mock).
    fn is_active(&self) -> bool {
        true
    }

    /// Apply per-utterance profile overrides. See
    /// [`TranscribeBackend::apply_profile_overrides`] for the contract; the
    /// production impl reads the `post_processor`, `post_mode`, `post_model`,
    /// `post_base_url`, `post_timeout_ms`, `post_max_input_chars`,
    /// `post_max_output_chars`, `post_redact`, and `post_redact_terms` keys
    /// so a per-app profile can point the post-processing pass at a
    /// different provider / model for one utterance.
    fn apply_profile_overrides(&self, _settings: &BTreeMap<String, String>) {}
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
    /// STT engine label surfaced on every utterance metrics/history row
    /// (Python's `stt_backend`: `"whisper"` for local, `"openai"` for
    /// cloud). Empty when the session is built from a bare
    /// [`Self::default`] (e.g. unit-test transcribe backends); production
    /// wiring stamps this from `VOICEPI_STT_BACKEND` at construction.
    /// Codex P1 #606 metrics-schema follow-up.
    pub stt_backend: String,
    /// Whisper model tag surfaced on every utterance row (Python's
    /// `model`: e.g. `"large-v3-turbo"`). Empty on the cloud path (the
    /// cloud request carries the caller-supplied `stt_model`) and on
    /// default-constructed test sessions. Codex P1 #606.
    pub model: String,
    /// Compute device surfaced on every utterance row (Python's
    /// `device`: `"cuda"` / `"cpu"` / `"auto"`). Empty on the cloud path
    /// and on default-constructed test sessions. Codex P1 #606.
    pub device: String,
    /// Compute precision surfaced on every utterance row (Python's
    /// `compute_type`: `"int8_float16"` / `"int8"` / `"float16"` /
    /// `"bfloat16"` / `"float32"`). Empty when the backend picks a
    /// default silently and on default-constructed test sessions.
    /// Codex P1 #606.
    pub compute_type: String,
    /// Which runtime served the utterance
    /// ([`crate::dictate::provenance::ENGINE_RUST_IN_PROCESS`] for this
    /// session; the Python worker stamps
    /// [`crate::dictate::provenance::ENGINE_PYTHON_WORKER`]). Emitted as
    /// the `engine` field on every utterance record so a log that shows
    /// BOTH runtimes starting (the Rust in-process dispatch plus a
    /// `python.exe -m whisper_dictate.runtime` line) still says
    /// unambiguously which one produced a given transcript. Empty on a
    /// default-constructed test session, which the wire emitter drops.
    pub engine: String,
    /// Injection strategy label surfaced on every utterance row (Python's
    /// `inject_mode`: `"auto"` / `"type"` / `"paste"` / `"print"`).
    /// The raw configured mode -- distinct from what the injector
    /// actually did (Python's `_last_inject_strategy`, which is out of
    /// scope for this pass; see the PR body). Empty on
    /// default-constructed test sessions. Codex P1 #606.
    pub inject_mode: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            min_record_seconds: 0.5,
            capture_backend: String::new(),
            audio_device: String::new(),
            capture_channels: 1,
            format_command_set: None,
            stt_backend: String::new(),
            model: String::new(),
            device: String::new(),
            compute_type: String::new(),
            engine: String::new(),
            inject_mode: String::new(),
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
