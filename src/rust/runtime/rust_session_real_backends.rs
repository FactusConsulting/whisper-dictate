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
use crate::dictate::backends::cloud_transcribe::{
    cloud_backend_local_only_checked, cloud_backend_requested_from_env,
};
use crate::dictate::backends::whisper_local::WhisperBackendConfig;
use crate::dictate::backends::WhisperLocalTranscribeBackend;
use crate::dictate::{
    CloudTranscribeConfig, DictateSession, ProductionTranscribeBackend, SessionConfig,
};
use crate::runtime::{RepaintNotifier, RuntimeEvent};
use crate::whisper::{
    parse_idle_timeout_from_env, resolve_model_path_from_env, IdleUnloadingModel,
};

use super::rust_session_inject::ProductionInjectBackend;

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

/// Env var that selects the spoken formatting-command set applied to
/// the final transcript before injection (`off` / `en` / `da` /
/// `both`). Mirrors `settings_schema.json`'s `format_commands` key so
/// the in-process rust-session path honours the same saved setting the
/// Python worker reads. The value flows into
/// [`crate::dictate::SessionConfig::format_command_set`]; empty / unset
/// resolves to `None`, which
/// [`crate::formatting::apply_format_commands`] treats as `off`.
pub(crate) const FORMAT_COMMANDS_ENV: &str = "VOICEPI_FORMAT_COMMANDS";

/// The real production session type that PR 5 wires behind
/// `VOICEPI_DICTATE_BACKEND=rust-session` when both features are on. The
/// transcribe seam is a [`ProductionTranscribeBackend`] so the runtime can
/// pick local Whisper or the cloud endpoint from `VOICEPI_STT_BACKEND`
/// (see [`make_real_session`]); the local variant is the
/// feature-gated [`WhisperLocalTranscribeBackend`].
pub(crate) type RealSession = DictateSession<
    ProductionTranscribeBackend<WhisperLocalTranscribeBackend>,
    ProductionInjectBackend,
>;

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
/// string. Pure helper so the parse is unit-testable. Codex P2 #423
/// rust_session_real_backends.rs:96 (finding 2).
pub(crate) fn whisper_backend_config_from_env() -> WhisperBackendConfig {
    let language = std::env::var(LANG_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let initial_prompt = std::env::var(INITIAL_PROMPT_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    WhisperBackendConfig {
        language,
        initial_prompt,
    }
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
        format_command_set: format_command_set_from_env(),
        ..SessionConfig::default()
    }
}

/// Read the spoken formatting-command set from [`FORMAT_COMMANDS_ENV`].
/// Empty / unset / whitespace-only normalises to `None` so the session
/// short-circuits to a passthrough; any other value is handed through
/// verbatim to [`crate::formatting::apply_format_commands`], whose own
/// `normalize_command_set` maps unknown/falsy tokens to `off`. Pure
/// helper so the parse is unit-testable without process env.
pub(crate) fn format_command_set_from_env() -> Option<String> {
    std::env::var(FORMAT_COMMANDS_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
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
/// - STT backend: [`cloud_backend_requested_from_env`] reads
///   `VOICEPI_STT_BACKEND`. `openai` selects the cloud
///   [`CloudTranscribeBackend`] (openai/Groq by base URL) built from
///   [`CloudTranscribeConfig::from_env`]; the model-path / idle-timeout
///   resolution below is SKIPPED on that path (cloud STT needs no local
///   model). Any other value keeps local Whisper.
/// - Model path (local only): [`resolve_model_path_from_env`] -- same
///   env-var / user-cache lookup the dispatcher and long-running server
///   use, so the contract is identical whether the user is on the
///   subprocess-per-utterance path, the long-running line server, or
///   the in-process Rust session.
/// - Idle timeout (local only): [`parse_idle_timeout_from_env`] -- same
///   `VOICEPI_WHISPER_IDLE_UNLOAD_S` knob.
/// - Whisper hints (local only): [`whisper_backend_config_from_env`]
///   (Codex P2 finding 2).
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
        // Transcribe seam: honour `VOICEPI_STT_BACKEND` the same way the
        // Python worker does. `openai` selects the cloud
        // `/audio/transcriptions` endpoint (openai OR Groq, by base URL) and
        // needs NO local model -- `ProductionTranscribeBackend::select`
        // runs the local thunk (and thus `resolve_model_path_from_env`)
        // ONLY on the local path, which is the whole point of cloud STT
        // (a user with no GGML model installed can still dictate). Any
        // other `VOICEPI_STT_BACKEND` value (incl. unset) keeps local
        // Whisper, the default. The selection logic is unit-tested in
        // `production_transcribe_tests.rs` (stock build).
        //
        // The cloud thunk enforces the local-only privacy lock FIRST
        // (`cloud_backend_local_only_checked`): under `VOICEPI_LOCAL_ONLY`
        // a non-loopback remote endpoint is refused so mic audio never
        // leaves the machine, matching the Python worker's
        // `_assert_local_backend` gate. On refusal the `Err` bubbles out of
        // `make_real_session` and the sink falls back to the stub session
        // (never silently POSTing audio remotely).
        // Dictionary support (Python parity, matching `simulate-session`):
        // term-based prompt biasing folds into the STT prompt for BOTH
        // backends here (`fold_into_prompt`), and the replacement table is
        // attached to the session below via `with_optional_dictionary`. Loaded
        // once from the same
        // `VOICEPI_DICTIONARY*` env + config the `dictionary-runtime` RPC reads;
        // disabled / empty is a no-op.
        let dictionary = crate::dictionary::load_session_dictionary();

        let transcribe = ProductionTranscribeBackend::select(
            cloud_backend_requested_from_env(),
            || {
                let mut config = CloudTranscribeConfig::from_env();
                dictionary.fold_into_prompt(&mut config.prompt);
                cloud_backend_local_only_checked(
                    crate::whisper::model_manager::is_local_only(),
                    config,
                )
            },
            || -> Result<WhisperLocalTranscribeBackend, String> {
                let model_path =
                    resolve_model_path_from_env().map_err(|e| format!("model path: {e:#}"))?;
                let idle =
                    parse_idle_timeout_from_env().map_err(|e| format!("idle timeout: {e:#}"))?;
                let model = IdleUnloadingModel::for_local_whisper(model_path, idle);
                let mut config = whisper_backend_config_from_env();
                dictionary.fold_into_prompt(&mut config.initial_prompt);
                Ok(WhisperLocalTranscribeBackend::new(model, config))
            },
        )?;

        // Inject backend reads VOICEPI_INJECT_MODE itself; the Print
        // variant short-circuits all OS calls. The Enigo variant
        // delegates to `EnigoInjectBackend::inject` which now owns
        // the modifier-release pre-step (Codex P2 #417 inject.rs:110).
        let inject = ProductionInjectBackend::from_env();

        // Attach the LLM post-processing pass when the operator configured
        // one (`VOICEPI_POST_PROCESSOR` != `none`). `from_env` returns None
        // for the default `none` processor, so a stock config installs no
        // backend and pays zero per-utterance cost. The pass runs before
        // the format-command layer inside the session (Python's
        // `postprocess -> format -> inject` order); `SessionPostProcess`
        // falls back to the raw transcript on any provider error, so this
        // can only improve output, never drop dictation.
        // Attach the LIVE-RELOADING dictionary replacement table (Python's
        // per-utterance `_dictionary_runtime`): the session re-reads config +
        // env + file(s) at each utterance boundary, so edits to the dictionary
        // or the `dictionary*` live settings take effect on the next utterance
        // without an app restart. ConfigFirst because in the live worker a
        // Settings save is the source of truth and the startup env is a stale
        // mirror. (The `dictionary` loaded above is used only for the one-shot
        // prompt fold; the session reloads its own replacement table.)
        let mut dictate = DictateSession::new(transcribe, inject, session_config_from_env())
            .with_reloading_dictionary(crate::dictionary::ReloadPrecedence::ConfigFirst);
        if let Some(post) = crate::postprocess::SessionPostProcess::from_env() {
            dictate = dictate.with_post_process(Box::new(post));
        }
        let session: Arc<Mutex<RealSession>> = Arc::new(Mutex::new(dictate));

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
