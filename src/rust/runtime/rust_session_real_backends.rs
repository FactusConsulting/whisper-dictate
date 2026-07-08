//! Wave 5 PR 5 of #348 -- construct the REAL
//! [`crate::dictate::backends::WhisperLocalTranscribeBackend`] +
//! [`super::rust_session_inject::ProductionInjectBackend`] session that
//! the coordinator-sink wiring drives when both the `whisper-rs-local`
//! and `rust-injection` features are compiled in.
//!
//! PR 4 (#416) installed the wiring with two stub backends so the
//! coordinator -> session -> worker-event loop was observable end-to-end
//! without pulling whisper.cpp or enigo into the dep graph. PR 5-prep
//! (#417) added the real trait impls (model loader, idle-unloader,
//! enigo dispatcher) but kept the production sink on the stubs. This
//! module is the small swap-in step: when the binary is compiled with
//! both features AND a model resolves successfully via
//! [`crate::whisper::resolve_model_path_from_env`], the supervisor's
//! [`super::rust_session_sink::build_production_sink`] returns a sink
//! backed by the real backends.
//!
//! # Round 2: Codex P1/P2 #423 findings
//!
//! Five Codex findings drove the round-2 follow-up:
//!
//! 1. **P1 audio routing** -- the original PR built real backends but
//!    no caller ever fed `push_frame` any audio. Fixed by spawning a
//!    [`super::rust_session_audio::AudioPump`] alongside the session
//!    and bundling it into [`RealSessionDeps`] so the coordinator
//!    sink's closure keeps the pump alive for the supervisor lifetime.
//! 2. **P2 Whisper hints** -- the original PR threw away
//!    `VOICEPI_LANG` + `VOICEPI_INITIAL_PROMPT`. Fixed by
//!    [`whisper_backend_config_from_env`] which reads both env vars
//!    and threads them into [`WhisperBackendConfig`].
//! 3. **P2 modifier release** -- now handled inside
//!    [`crate::dictate::backends::EnigoInjectBackend::inject`] itself
//!    (Codex P2 #417 inject.rs:110 follow-up, PR #419), so no
//!    additional wrapping is needed here. The
//!    [`super::rust_session_inject::ProductionInjectBackend::Enigo`]
//!    variant delegates straight through.
//! 4. **P2 print mode** -- new
//!    [`super::rust_session_inject::ProductionInjectBackend`] wrapper
//!    honors `VOICEPI_INJECT_MODE=print` by skipping OS injection.
//! 5. **P2 min-record floor** -- [`session_config_from_env`] now
//!    sources `min_record_seconds` from
//!    [`crate::dictate::audio_route::RouteConfig::from_env`] (the
//!    single source of truth for the parse semantics).
//!
//! # Gating
//!
//! The whole module is `#[cfg(all(feature = "whisper-rs-local", feature
//! = "rust-injection"))]` -- default builds compile zero new code from
//! this PR, and a build with only one feature still falls through to
//! the PR 4 stub path. End-user impact is therefore opt-in twice:
//!
//! 1. Pass
//!    `--features whisper-rs-local,rust-injection,rust-hotkeys,audio-in-rust`
//!    at build time (the `audio-in-rust` feature is required for the
//!    audio pump that addresses finding 1 -- without it
//!    [`make_real_session`] returns an `Err` so the sink falls back to
//!    the PR 4 stubs with a stderr warning).
//! 2. Set `VOICEPI_DICTATE_BACKEND=rust-session` at run time.
//!
//! Without (1) the call to [`make_real_session`] does not exist;
//! without (2) `dictate_backend_rust_session_requested()` returns false
//! and the supervisor installs the historical logger sink instead.
//!
//! # Why this lives in its own module
//!
//! `rust_session_sink.rs` is already in the ~400-LOC range; adding the
//! real-backend constructor inline would push it past the 500-LOC
//! modularity guideline (AGENTS.md "Modularity"). Splitting also
//! isolates the heavy whisper.cpp / enigo deps behind a single cfg gate
//! so a default build does not even parse the real backend types.
//!
//! # Deferred to follow-up PRs
//!
//! The PR 5-prep backends today wire `transcribe -> inject` directly.
//! The full Python flow from `vp_dictate.py:431-491` also runs
//! `postprocess::run::run()` -> `formatting::apply_format_commands` ->
//! per-utterance health-line bookkeeping between transcription and
//! injection. That chaining is out of scope for THIS PR (per the
//! Wave 5 slicing plan) -- see issue follow-up `wave5-pr5-postprocess`
//! (filed by this PR).

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::dictate::audio_route::RouteConfig;
use crate::dictate::backends::whisper_local::WhisperBackendConfig;
use crate::dictate::backends::{CloudBackendConfig, CloudTranscribeBackend, WhisperLocalTranscribeBackend};
use crate::dictate::{
    DictateSession, SessionConfig, TranscribeBackend, TranscribeError, TranscribeResult,
};
use crate::runtime::{RepaintNotifier, RuntimeEvent};
use crate::whisper::{
    parse_idle_timeout_from_env, resolve_model_path_from_env, IdleUnloadingModel,
};

use super::rust_session_inject::ProductionInjectBackend;

/// Env var carrying the STT backend selection. Mirrors
/// `vp_transcribe.STT_BACKEND` on the Python side. Recognised values
/// (case-insensitive, `faster-whisper` normalised to `whisper` per
/// [`crate::dictate::validate_backend`]):
///
/// - `"whisper"` (or unset / empty) -- local whisper.cpp inference.
/// - `"openai"` -- OpenAI-compatible cloud STT (base URL determines
///   OpenAI vs. Groq vs. self-hosted).
///
/// A user with `stt_provider = "groq"` and the Groq base URL saved
/// still sets `stt_backend = "openai"` -- provider is a sub-choice of
/// the cloud backend, so we only branch here on the top-level backend
/// selection. See [`resolve_stt_backend_from_env`] for the parser.
pub(crate) const STT_BACKEND_ENV: &str = "VOICEPI_STT_BACKEND";

/// Result of [`resolve_stt_backend_from_env`]: which real backend the
/// factory should build. Kept separate from the `dictate::BackendKind`
/// enum so this layer can grow provider-specific variants (Cloud vs.
/// Whisper is enough for Wave 5.5 gap #1; a future Wave could split
/// Cloud into OpenAI / Groq if the wire divergence ever requires it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealBackendKind {
    /// In-process whisper.cpp inference via
    /// [`WhisperLocalTranscribeBackend`]. Default when the env var is
    /// unset -- matches the Python worker's fallback.
    Whisper,
    /// OpenAI-compatible cloud STT via [`CloudTranscribeBackend`].
    Cloud,
}

/// Parse [`STT_BACKEND_ENV`] into a [`RealBackendKind`]. Recognises the
/// same historical `faster-whisper` alias `vp_transcribe.py` does. An
/// unset or blank env var lands on [`RealBackendKind::Whisper`] --
/// production defaults to local inference just like the Python worker.
/// Unrecognised values return `None` so the factory can surface a
/// human-readable "unsupported stt_backend" error rather than falling
/// through to a silent whisper build.
pub(crate) fn resolve_stt_backend_from_env() -> Option<RealBackendKind> {
    let raw = std::env::var(STT_BACKEND_ENV).ok().unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "whisper" | "faster-whisper" => Some(RealBackendKind::Whisper),
        "openai" | "groq" => Some(RealBackendKind::Cloud),
        _ => None,
    }
}

/// Real production [`TranscribeBackend`] impl for the rust-session
/// worker. Enum dispatch (rather than `Box<dyn>`) so the two variants
/// share a single monomorphised session type and clippy's dead-code
/// lint doesn't fire on the always-live `TranscribeBackend for
/// DictateSession<...>` bound.
///
/// Wave 5.5 gap #1 of #348 -- see the [`super::rust_session_real_backends`]
/// module docs for the "silent stub fallback for cloud STT" bug this
/// enum closes.
pub enum RealTranscribeBackend {
    /// In-process whisper.cpp via [`WhisperLocalTranscribeBackend`].
    Whisper(WhisperLocalTranscribeBackend),
    /// OpenAI-compatible cloud STT via [`CloudTranscribeBackend`].
    Cloud(CloudTranscribeBackend),
}

impl TranscribeBackend for RealTranscribeBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        match self {
            Self::Whisper(inner) => inner.transcribe(pcm, sample_rate),
            Self::Cloud(inner) => inner.transcribe(pcm, sample_rate),
        }
    }
}

/// Env var that supplies the spoken-language hint for the local
/// Whisper backend. Mirrors `vp_cli.py` / `settings_schema.json:89` so
/// the rust-session path honors the same saved setting the Python
/// worker reads. Codex P2 #423 rust_session_real_backends.rs:96
/// (finding 2).
pub(crate) const LANG_ENV: &str = "VOICEPI_LANG";

/// Env var that supplies the initial-prompt vocabulary hint for the
/// local Whisper backend. Mirrors `vp_cli.py` /
/// `settings_schema.json:107`. Codex P2 #423
/// rust_session_real_backends.rs:96 (finding 2).
pub(crate) const INITIAL_PROMPT_ENV: &str = "VOICEPI_INITIAL_PROMPT";

/// The real production session type that PR 5 wires behind
/// `VOICEPI_DICTATE_BACKEND=rust-session` when both features are on.
///
/// Wave 5.5 gap #1 of #348 widened this from a fixed
/// [`WhisperLocalTranscribeBackend`] to the [`RealTranscribeBackend`]
/// enum so the same `DictateSession` type owns either the local
/// whisper.cpp path or the OpenAI-compatible cloud path -- picked by
/// [`resolve_stt_backend_from_env`] at session construction.
pub(crate) type RealSession =
    DictateSession<RealTranscribeBackend, ProductionInjectBackend>;

/// Bundle handed back from [`make_real_session`].
///
/// Holding the [`super::rust_session_audio::AudioPump`] (when
/// constructed) alongside the session keeps the cpal stream + pump
/// thread alive for the caller's lifetime. The coordinator-sink
/// closure moves the whole bundle into its captures so the pump lives
/// for as long as the sink does; dropping the bundle stops the
/// pipeline + joins the pump thread (see
/// [`super::rust_session_audio::AudioPump`]'s `Drop` impl).
pub(crate) struct RealSessionDeps {
    pub(crate) session: Arc<Mutex<RealSession>>,
    /// The live audio pump. Only present when the `audio-in-rust`
    /// feature is compiled in (which is also a precondition for
    /// [`make_real_session`] succeeding -- without the feature the
    /// constructor returns an `Err` before reaching this struct).
    /// Stored on the struct so the sink can keep it alive without
    /// having to know about the cfg gate.
    ///
    /// `#[allow(dead_code)]` because the field is never *read* in
    /// this module -- its only purpose is to keep the cpal stream +
    /// pump thread alive via the struct's `Drop`. The caller
    /// (`build_production_sink`) moves the whole struct into a
    /// closure capture; clippy's dead-code lint would otherwise
    /// flag the field because nothing dereferences it.
    #[cfg(feature = "audio-in-rust")]
    #[allow(dead_code)]
    pub(crate) audio: super::rust_session_audio::AudioPump,
}

/// Read [`WhisperBackendConfig`] from the same `VOICEPI_LANG` +
/// `VOICEPI_INITIAL_PROMPT` env vars `vp_cli.py` honors. Empty / unset
/// values are normalised to `None` so the backend's own per-call
/// empty-string -> auto-detect collapse (see
/// [`WhisperBackendConfig`] docs) does not even see a literal empty
/// string.
///
/// Wave 5.5 gap #4: when the user's dictionary is enabled (the default
/// for a stock install), overlay the dictionary-derived initial-prompt
/// on top of the env-var base. `Dictionary::build_prompt` combines the
/// base prompt with the budget-fitted vocabulary terms; without this,
/// the rust-session path threw the dictionary away and got zero
/// rare-word bias from whisper, silently regressing recognition
/// against the Python worker.
///
/// Pure helper so the parse is unit-testable. Codex P2 #423
/// rust_session_real_backends.rs:96 (finding 2).
pub(crate) fn whisper_backend_config_from_env() -> WhisperBackendConfig {
    let language = std::env::var(LANG_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let base_prompt = std::env::var(INITIAL_PROMPT_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let initial_prompt = build_initial_prompt(base_prompt.as_deref());
    WhisperBackendConfig {
        language,
        initial_prompt,
    }
}

/// Combine an optional env-var base prompt with the dictionary's
/// vocabulary terms into the final `initial_prompt` string whisper.cpp
/// consumes. Extracted so the fallback logic (dictionary disabled ->
/// pass the env prompt through; both missing -> `None`) is
/// unit-testable without touching the process env.
///
/// The dictionary module owns the budget-fitting + "Vocabulary:" prefix
/// so this helper is a thin adapter around
/// [`crate::dictionary::runtime_dictionary_result`]. When the runtime
/// helper returns an error (missing / corrupt dictionary file), we
/// fall back to the base prompt so a broken dictionary never blocks
/// the session from starting -- mirrors the Python
/// `_dictionary_runtime` fallback path.
pub(crate) fn build_initial_prompt(base_prompt: Option<&str>) -> Option<String> {
    let settings = crate::dictionary::RuntimeDictionarySettings::from_env_and_config();
    let result = crate::dictionary::runtime_dictionary_result(&settings, base_prompt, "");
    result.prompt
}

/// Build a [`SessionConfig`] that honors the live
/// `VOICEPI_MIN_RECORD_SECONDS` setting. Mirrors the audio_route's own
/// env parser ([`RouteConfig::from_env`]) so both layers see the same
/// value -- when the supervisor wires audio_route in a future PR the
/// route will additionally re-read the env on every
/// `start_recording`, but for the rust-session sink today the
/// construction-time stamp is enough to fix Codex P2 #423
/// rust_session_real_backends.rs:107 (finding 5).
pub(crate) fn session_config_from_env() -> SessionConfig {
    let route = RouteConfig::from_env();
    SessionConfig {
        min_record_seconds: route.min_record_seconds,
        ..SessionConfig::default()
    }
}

/// Construct the [`RealTranscribeBackend`] variant matching `kind`,
/// pulling every dependent config value from env. Split out of
/// [`make_real_session`] so the two per-backend builders (model + idle
/// wrapper for Whisper, base URL + key + model for Cloud) each stay in
/// their own linear block and the routing test can drive the
/// dispatcher without the audio-pump gate.
///
/// Returns `Err(String)` mirroring [`make_real_session`]'s error
/// contract -- the supervisor logs the message and falls back to
/// the stub sink.
pub(crate) fn build_real_transcribe_backend(
    kind: RealBackendKind,
) -> Result<RealTranscribeBackend, String> {
    match kind {
        RealBackendKind::Whisper => {
            let model_path =
                resolve_model_path_from_env().map_err(|e| format!("model path: {e:#}"))?;
            let idle =
                parse_idle_timeout_from_env().map_err(|e| format!("idle timeout: {e:#}"))?;
            let model = IdleUnloadingModel::for_local_whisper(model_path, idle);
            Ok(RealTranscribeBackend::Whisper(
                WhisperLocalTranscribeBackend::new(model, whisper_backend_config_from_env()),
            ))
        }
        RealBackendKind::Cloud => {
            let cfg = CloudBackendConfig::from_env();
            let backend = CloudTranscribeBackend::new(cfg).map_err(|e| format!("cloud STT: {e}"))?;
            Ok(RealTranscribeBackend::Cloud(backend))
        }
    }
}

/// Build the real-backend session, wrapped in `Arc<Mutex<...>>` so the
/// coordinator-sink closure can hold a clone while exposing a separate
/// clone for tests / supervisor introspection. The returned struct
/// additionally carries the live audio pump so the supervisor only has
/// to keep the bundle alive for the rust-session path to actually
/// capture audio (Codex P1 #423 finding 1).
///
/// Resolution rules:
///
/// - Model path: [`resolve_model_path_from_env`] -- same env-var /
///   user-cache lookup the dispatcher and long-running server use, so
///   the contract is identical whether the user is on the
///   subprocess-per-utterance path, the long-running line server, or
///   the in-process Rust session.
/// - Idle timeout: [`parse_idle_timeout_from_env`] -- same
///   `VOICEPI_WHISPER_IDLE_UNLOAD_S` knob.
/// - Whisper hints: [`whisper_backend_config_from_env`] (Codex P2
///   finding 2).
/// - Inject mode: [`ProductionInjectBackend::from_env`] (Codex P2
///   finding 4).
/// - Min-record floor: [`session_config_from_env`] (Codex P2 finding 5).
///
/// `tx` + `repaint_notifier` are threaded down to the audio pump so
/// device errors surface on the runtime event channel and wake the
/// egui UI on minimised-window installs.
///
/// Returns `Err(String)` (rather than a typed error) so the caller can
/// log the message and fall back to the stub session without having to
/// learn the union of underlying error types. The caller treats the
/// string as human-readable and surfaces it on the runtime event
/// channel.
pub(crate) fn make_real_session(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> Result<RealSessionDeps, String> {
    // `audio-in-rust` is required for finding 1 (the audio pump). On a
    // build without it we surface a human-readable warning so the
    // supervisor's stub-fallback path includes the actionable hint.
    #[cfg(not(feature = "audio-in-rust"))]
    {
        // Silence "unused" warnings on the non-audio build: `tx` /
        // `repaint_notifier` are only consumed by the audio pump.
        let _ = (tx, repaint_notifier);
        Err(
            "audio-in-rust feature not compiled in; rebuild with `--features \
             whisper-rs-local,rust-injection,rust-hotkeys,audio-in-rust` to \
             enable the real rust-session path (Codex P1 #423 finding 1)"
                .to_owned(),
        )
    }
    #[cfg(feature = "audio-in-rust")]
    {
        // Wave 5.5 gap #1 of #348: branch on VOICEPI_STT_BACKEND so a
        // `stt_backend = "openai"` install builds the cloud backend
        // instead of the local Whisper one. Unrecognised backend values
        // surface as an explicit error rather than silently falling
        // back to whisper -- an unsupported value in the env is far
        // more likely a misconfigured worker than an intentional
        // opt-in.
        let backend_kind = resolve_stt_backend_from_env().ok_or_else(|| {
            format!(
                "unsupported {STT_BACKEND_ENV} value; expected one of whisper, openai, groq"
            )
        })?;
        let transcribe = build_real_transcribe_backend(backend_kind)?;

        // Inject backend reads VOICEPI_INJECT_MODE itself; the Print
        // variant short-circuits all OS calls. The Enigo variant
        // delegates to `EnigoInjectBackend::inject` which now owns
        // the modifier-release pre-step (Codex P2 #417 inject.rs:110).
        let inject = ProductionInjectBackend::from_env();

        let session: Arc<Mutex<RealSession>> = Arc::new(Mutex::new(DictateSession::new(
            transcribe,
            inject,
            session_config_from_env(),
        )));

        // Spawn the audio pump LAST so a model-path / idle-timeout
        // parse failure does not leak the cpal stream + Silero
        // worker. Pump construction itself is fail-fast: if cpal /
        // Silero refuse to start the supervisor's stderr surfaces the
        // error and the sink falls back to stubs.
        let audio = super::rust_session_audio::AudioPump::spawn_for_session(
            Arc::clone(&session),
            tx,
            repaint_notifier,
        )
        .map_err(|e| format!("audio pump: {e:#}"))?;

        Ok(RealSessionDeps { session, audio })
    }
}

#[cfg(test)]
#[path = "rust_session_real_backends_tests.rs"]
mod tests;
