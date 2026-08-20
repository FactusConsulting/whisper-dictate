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
//!    [`super::rust_session_inject::ProductionInjectBackend`]'s Enigo
//!    arm delegates straight through.
//! 4. **P2 print mode** -- new
//!    [`super::rust_session_inject::ProductionInjectBackend`] wrapper
//!    honors `VOICEPI_INJECT_MODE=print` by skipping OS injection.
//! 5. **P2 min-record floor** -- [`session_config_from_env`] sources
//!    `min_record_seconds` from the live runtime environment.
//!
//! # Gating
//!
//! The whole module is `#[cfg(all(feature = "whisper-rs-local", feature
//! = "rust-injection"))]` -- default builds compile zero new code from
//! this PR, and a build with only one feature still falls through to
//! the PR 4 stub path. End-user impact is therefore opt-in twice:
//!
//! 1. Pass
//!    `--no-default-features --features shipping`
//!    at build time (the `audio-capture` feature is required for the
//!    audio pump -- without it
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

use crate::dictate::backends::cloud_transcribe::{STT_BACKEND_CLOUD, STT_MODEL_ENV};
use crate::dictate::backends::whisper_local::WhisperBackendConfig;
use crate::dictate::backends::WhisperLocalTranscribeBackend;
use crate::dictate::{
    CloudTranscribeConfig, DictateSession, PreviewBackend, PreviewEngine, PreviewEngineConfig,
    ProductionTranscribeBackend, SessionConfig,
};
use crate::runtime::{RepaintNotifier, RuntimeEvent};

use super::rust_session_preview::runtime_channel_preview_sink;
use crate::whisper::{resolve_model_path, IdleUnloadingModel};

use super::rust_session_inject::ProductionInjectBackend;
use super::settings_snapshot::RuntimeSettingsSnapshot;

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

/// Live minimum utterance duration setting.
pub(crate) const MIN_RECORD_ENV: &str = "VOICEPI_MIN_RECORD_SECONDS";
const DEFAULT_MIN_RECORD_S: f64 = 0.5;
pub(crate) const MAX_RECORD_ENV: &str = "VOICEPI_MAX_RECORD_S";
const DEFAULT_MAX_RECORD_S: f64 = 120.0;
pub(crate) const COMMAND_HOOK_ENV: &str = "VOICEPI_COMMAND_HOOK";
pub(crate) const COMMAND_HOOK_TIMEOUT_ENV: &str = "VOICEPI_COMMAND_HOOK_TIMEOUT_MS";

/// Env var that controls the live partial-transcription preview interval
/// (`vp_preview.py`'s `preview_seconds`). `0` disables the preview
/// entirely. Mirrors `settings_schema.json`'s `preview_seconds` key so the
/// in-process rust-session path honours the same saved setting the Python
/// worker reads. Only meaningful on the LOCAL Whisper backend -- the cloud
/// backend never wires a preview regardless (matching Python's
/// `PREVIEW_BACKENDS = ("whisper",)` cloud-cost guard).
pub(crate) const PREVIEW_SECONDS_ENV: &str = "VOICEPI_PREVIEW_SECONDS";

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
    /// Independent capture close handle used by the supervisor before it
    /// reports Stopped. The owning sink may remain blocked in transcription.
    pub(crate) capture_stop: super::supervisor::CaptureStop,
    /// The live audio pump. Only present when the `audio-capture`
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
    #[cfg(feature = "audio-capture")]
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
#[cfg(test)]
pub(crate) fn whisper_backend_config_from_env() -> WhisperBackendConfig {
    whisper_backend_config_with(|name| std::env::var(name).ok())
}

fn whisper_backend_config_with(lookup: impl Fn(&str) -> Option<String>) -> WhisperBackendConfig {
    let get = |name: &str| {
        lookup(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    WhisperBackendConfig {
        language: get(LANG_ENV),
        initial_prompt: get(INITIAL_PROMPT_ENV),
    }
}

/// Build a [`SessionConfig`] that honors the live
/// `VOICEPI_MIN_RECORD_SECONDS` setting.
///
/// Also stamps the STT / device / inject_mode labels the metrics +
/// history sinks emit on every utterance (`stt_backend`, `model`,
/// `device`, `inject_mode`). These fields are
/// construction-time stamps rather than live-reloaded because they
/// require rebuilding the backend anyway (a `stt_backend` flip switches
/// local <-> cloud; a `model` swap unloads/reloads GGML weights) --
/// matching Python, which restarts the worker for the same set of
/// keys (`RESTART_KEYS`). Codex P1 #606 metrics-schema follow-up.
#[cfg(test)]
pub(crate) fn session_config_from_env() -> SessionConfig {
    session_config_with(|name| std::env::var(name).ok())
}

fn session_config_with(lookup: impl Fn(&str) -> Option<String>) -> SessionConfig {
    // Resolve stt_backend + model TOGETHER via the same selection
    // `make_real_session` uses. Previously both fields were read from
    // `VOICEPI_STT_BACKEND` / `VOICEPI_MODEL` verbatim, which produced
    // two schema-level lies in history/metrics rows:
    //
    // * When `VOICEPI_STT_BACKEND=openai`, the row's `model` field was
    //   populated from the local-only `VOICEPI_MODEL` env var, so a
    //   cloud request looked like it had used `large-v3-turbo` when it
    //   actually used `gpt-4o-transcribe` (Codex P2 #620
    //   rust_session_real_backends.rs:221 —
    //   "Label cloud events with the cloud model").
    // * A noncanonical `VOICEPI_STT_BACKEND=OPENAI` or stale
    //   `parakeet` / `faster-whisper` value flowed straight to disk
    //   even though `cloud_backend_requested_from_env` normalises to
    //   `openai` / local Whisper (Codex P2 #620
    //   rust_session_real_backends.rs:220 —
    //   "Canonicalize the backend label from the selected backend").
    //
    // Deriving both labels from the same `cloud_backend_requested_from_env`
    // gate that selects the backend keeps the schema row honest and
    // guarantees writer/reader agreement across the migration.
    let cloud = lookup(crate::dictate::backends::cloud_transcribe::STT_BACKEND_ENV)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(STT_BACKEND_CLOUD));
    let (stt_backend, model) = if cloud {
        (
            STT_BACKEND_CLOUD.to_owned(),
            env_string_with(STT_MODEL_ENV, &lookup),
        )
    } else {
        (
            "whisper".to_owned(),
            env_string_with("VOICEPI_MODEL", &lookup),
        )
    };
    SessionConfig {
        min_record_seconds: parse_min_record_seconds(lookup(MIN_RECORD_ENV).as_deref()),
        max_record_seconds: Some(parse_max_record_seconds(lookup(MAX_RECORD_ENV).as_deref())),
        format_command_set: format_command_set_with(&lookup),
        stt_backend,
        model,
        device: env_string_with("VOICEPI_DEVICE", &lookup),
        // whisper.cpp quantisation is encoded in the model file.
        compute_type: String::new(),
        // Fixed for this module by construction: everything built here
        // runs inside `whisper-dictate.exe`. Stamped so an utterance
        // record is self-describing even when the diagnostic log shows a
        // Python worker starting in the same session.
        engine: crate::dictate::provenance::ENGINE_RUST_IN_PROCESS.to_owned(),
        inject_mode: env_string_with("VOICEPI_INJECT_MODE", &lookup),
        command_hook: env_string_with(COMMAND_HOOK_ENV, &lookup),
        command_hook_timeout_ms: parse_command_hook_timeout(
            lookup(COMMAND_HOOK_TIMEOUT_ENV).as_deref(),
        ),
        ..SessionConfig::default()
    }
}

#[cfg(test)]
fn min_record_seconds_from_env() -> f64 {
    let raw = std::env::var(MIN_RECORD_ENV).ok();
    parse_min_record_seconds(raw.as_deref())
}

fn parse_min_record_seconds(raw: Option<&str>) -> f64 {
    let parsed = raw
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_MIN_RECORD_S);
    if parsed.is_finite() && parsed > 0.0 {
        parsed
    } else if parsed.is_finite() {
        0.0
    } else {
        DEFAULT_MIN_RECORD_S
    }
}

fn parse_max_record_seconds(raw: Option<&str>) -> f64 {
    match raw.and_then(|value| value.trim().parse::<f64>().ok()) {
        Some(value) if value.is_finite() && value >= 0.0 => value,
        _ => DEFAULT_MAX_RECORD_S,
    }
}

fn parse_command_hook_timeout(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.max(1.0) as u64)
        .unwrap_or(2_000)
}

/// The `(stt_impl, stt_accel)` pair the startup banner should report for
/// the transcribe backend that was actually constructed.
///
/// Both halves come from the built backend, never from env:
///
/// * **Local** -- `whisper.cpp` plus the process-wide accelerator verdict.
///   At startup that is still the PLAN (env GPU policy + compiled-in
///   backend), because whisper.cpp loads its model lazily on the first
///   utterance and has not yet had a chance to disagree. The
///   authoritative per-load verdict is logged by
///   `crate::whisper::local::LocalWhisper` (`[whisper] model loaded: ...
///   accel=...`) and stamped on every utterance record as `stt_accel`;
///   when the two disagree, the utterance record tells the truth.
/// * **Cloud** -- the provider sniffed from the live base URL, and
///   `unknown` for the accelerator. The local whisper.cpp GPU plan says
///   nothing about a remote provider's compute path, and reporting it
///   would announce `impl=cloud-openai accel=vulkan` on a Vulkan build
///   while the utterance records for that same session correctly say
///   `unknown`. Codex P2 #687 rust_session_real_backends.rs:287.
fn startup_provenance_for(
    transcribe: &ProductionTranscribeBackend<WhisperLocalTranscribeBackend>,
) -> (&'static str, &'static str) {
    match transcribe {
        ProductionTranscribeBackend::Local(_) => (
            crate::dictate::provenance::STT_IMPL_WHISPER_CPP,
            crate::whisper::accel::global().resolved().as_str(),
        ),
        ProductionTranscribeBackend::Cloud(cloud) => {
            (cloud.stt_impl(), crate::whisper::Accel::Unknown.as_str())
        }
    }
}

/// Render the startup provenance line for a session built from `config`
/// against the `stt_impl` / `stt_accel` pair
/// [`startup_provenance_for`] resolved from the constructed backend.
///
/// Answers "what am I actually running" at a glance, once, at startup:
///
/// ```text
/// [runtime] transcribe backend resolved: engine=rust-in-process impl=whisper.cpp accel=vulkan model=large-v3-turbo
/// ```
///
/// Pure formatter -- nothing is re-derived from env here, so the line
/// cannot drift from the backend that was really built.
pub(crate) fn startup_provenance_line(
    config: &SessionConfig,
    stt_impl: &str,
    stt_accel: &str,
) -> String {
    crate::dictate::provenance::startup_line(&config.engine, stt_impl, stt_accel, &config.model)
}

/// Trim + collapse-empty helper: an unset / whitespace-only env var
/// returns the empty string, which the wire emitter's
/// [`crate::dictate::session::wire::insert_non_empty`] then drops from
/// the utterance payload so a partial config never emits blank
/// `"model": ""` rows.
#[cfg(test)]
fn env_string(key: &str) -> String {
    env_string_with(key, &|name| std::env::var(name).ok())
}

fn env_string_with(key: &str, lookup: &impl Fn(&str) -> Option<String>) -> String {
    lookup(key).map(|v| v.trim().to_owned()).unwrap_or_default()
}

/// Read the spoken formatting-command set from [`FORMAT_COMMANDS_ENV`].
/// Empty / unset / whitespace-only normalises to `None` so the session
/// short-circuits to a passthrough; any other value is handed through
/// verbatim to [`crate::formatting::apply_format_commands`], whose own
/// `normalize_command_set` maps unknown/falsy tokens to `off`. Pure
/// helper so the parse is unit-testable without process env.
#[cfg(test)]
pub(crate) fn format_command_set_from_env() -> Option<String> {
    format_command_set_with(&|name| std::env::var(name).ok())
}

fn format_command_set_with(lookup: &impl Fn(&str) -> Option<String>) -> Option<String> {
    lookup(FORMAT_COMMANDS_ENV)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Resolve the live-preview interval from [`PREVIEW_SECONDS_ENV`]. `0` or
/// negative disables the preview; unset defaults to `3` seconds, matching
/// `settings_schema.json`'s `preview_seconds` default. Whitespace-only /
/// unparseable values are treated as "unset" (default `3`), matching Python's
/// `float(effective_config.get("preview_seconds", "3"))` behaviour when the
/// key is missing.
#[cfg(test)]
pub(crate) fn preview_seconds_from_env() -> f64 {
    preview_seconds_with(&|name| std::env::var(name).ok())
}

fn preview_seconds_with(lookup: &impl Fn(&str) -> Option<String>) -> f64 {
    lookup(PREVIEW_SECONDS_ENV)
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .unwrap_or(3.0)
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
    make_real_session_with_activity(
        tx,
        repaint_notifier,
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    )
}

pub(crate) fn make_real_session_with_activity(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
    runtime_active: Arc<std::sync::atomic::AtomicBool>,
) -> Result<RealSessionDeps, String> {
    let runtime = RuntimeSettingsSnapshot::from_pairs_with_ambient(
        crate::config::worker_env_overrides(),
        |name| std::env::var(name).ok(),
    )
    .map_err(|err| format!("runtime settings: {err}"))?;
    make_real_session_with_activity_and_settings(
        tx,
        repaint_notifier,
        runtime_active,
        &runtime,
        None,
    )
}

pub(crate) fn make_real_session_with_activity_and_settings(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
    runtime_active: Arc<std::sync::atomic::AtomicBool>,
    runtime: &RuntimeSettingsSnapshot,
    config_path: Option<&std::path::Path>,
) -> Result<RealSessionDeps, String> {
    let stt_provider = config_path
        .and_then(|path| crate::config::load_raw_config_from_path(path).ok())
        .and_then(|raw| crate::config::AppSettings::from_value(raw).ok())
        .map(|settings| settings.stt_provider)
        .unwrap_or_else(|| runtime.stt_provider().to_owned());
    // `audio-capture` is required for the audio pump. On a
    // build without it we surface a human-readable warning so the
    // supervisor's stub-fallback path includes the actionable hint.
    #[cfg(not(feature = "audio-capture"))]
    {
        // Silence "unused" warnings on the non-audio build: `tx` /
        // `repaint_notifier` are only consumed by the audio pump.
        let _ = (tx, repaint_notifier, runtime_active, config_path);
        Err("audio-capture feature not compiled in; rebuild with \
             `--no-default-features --features shipping` to \
             enable the real native session"
            .to_owned())
    }
    #[cfg(feature = "audio-capture")]
    {
        let settings = runtime.settings();
        let lookup = |name: &str| runtime.value(name).map(str::to_owned);
        crate::diag::configure_level(&settings.log_level);
        let dictionary_settings =
            crate::dictionary::RuntimeDictionarySettings::from_app_settings(settings);
        let transcription_guards =
            crate::dictate::backends::hallucination::TranscriptionGuards::from_lookup(&lookup);
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
        // Dictionary support (Python parity, matching `simulate-session`), both
        // halves LIVE-reloaded per utterance: the term-based prompt biasing is
        // re-folded into the STT prompt by the backend (`with_reloading_prompt`)
        // and the replacement table is re-read by the session
        // (`with_reloading_dictionary`, below). `ConfigFirst` because in the
        // live worker a Settings save (config.json, no restart) is the source of
        // truth and the startup env is a stale mirror. Each backend keeps its
        // config's prompt field as the raw base (`VOICEPI_INITIAL_PROMPT`); the
        // reloading prompt folds the current dictionary terms into it each call.
        let transcribe = ProductionTranscribeBackend::select(
            settings
                .stt_backend
                .trim()
                .eq_ignore_ascii_case(STT_BACKEND_CLOUD),
            || {
                let config = CloudTranscribeConfig::from_env_with(&lookup);
                let backend = if stt_provider.trim().eq_ignore_ascii_case("nemotron") {
                    crate::dictate::CloudTranscribeBackend::new_nemotron(config)
                } else {
                    crate::dictate::CloudTranscribeBackend::new(config)
                };
                crate::privacy::assert_local_backend(
                    settings.local_only,
                    STT_BACKEND_CLOUD,
                    "STT",
                    Some(&backend.config().base_url),
                )
                .map_err(|e| format!("{e:#}"))
                .map(|_| backend)
                .map(|backend| {
                    backend
                        .with_reloading_prompt_settings(dictionary_settings.clone())
                        .with_transcription_guards(transcription_guards)
                })
            },
            || -> Result<WhisperLocalTranscribeBackend, String> {
                let model_path = resolve_model_path(
                    runtime.value(crate::whisper::MODEL_PATH_ENV),
                    Some(&settings.model),
                )
                .map_err(|e| format!("model path: {e:#}"))?;
                let idle = runtime
                    .value(crate::whisper::IDLE_UNLOAD_ENV)
                    .map(crate::whisper::idle::parse_idle_timeout_str)
                    .transpose()
                    .map_err(|e| format!("idle timeout: {e:#}"))?
                    .flatten();
                let gpu_policy = crate::whisper::gpu::parse_gpu_policy(
                    runtime.value(crate::whisper::GPU_ENV),
                    Some(&settings.device),
                )
                .map_err(|e| format!("GPU policy: {e:#}"))?;
                let model =
                    IdleUnloadingModel::for_local_whisper_with_policy(model_path, idle, gpu_policy);
                let config = whisper_backend_config_with(&lookup);
                Ok(WhisperLocalTranscribeBackend::new(model, config)
                    .with_reloading_prompt_settings(dictionary_settings.clone())
                    .with_transcription_guards(transcription_guards))
            },
        )?;

        // Inject backend reads VOICEPI_INJECT_MODE itself; the Print
        // variant short-circuits all OS calls. The Enigo variant
        // delegates to `EnigoInjectBackend::inject` which now owns
        // the modifier-release pre-step (Codex P2 #417 inject.rs:110).
        let inject = ProductionInjectBackend::from_settings_with_activity(
            &settings.inject_mode,
            (!settings.xkb_layout.trim().is_empty()).then_some(settings.xkb_layout.as_str()),
            Arc::clone(&runtime_active),
        )
        .map_err(|err| format!("inject backend: {err}"))?;

        // Live partial-transcription preview: only wired on the LOCAL
        // Whisper backend (Python parity: `PREVIEW_BACKENDS = ("whisper",)`),
        // and only when the operator has not disabled it via
        // `VOICEPI_PREVIEW_SECONDS=0`. The preview shares this backend's
        // resident model instance through `share_for_preview()` so no
        // second copy of the GGML weights loads into RAM; the wrapper's
        // internal `Mutex<Option<M>>` serialises preview / final passes
        // exactly like Python's `TRANSCRIBE_LOCK`. The cloud arm always
        // yields `None` -- previews there would spam a paid API.
        //
        // Closes parity blocker #4 (engine-assessment list). See
        // `crate::dictate::session::preview` for the cadence, fresh-audio
        // gate, sliding-window cap, and stop-suppression contract.
        let preview_engine = match &transcribe {
            ProductionTranscribeBackend::Local(local) => {
                PreviewEngineConfig::from_seconds(preview_seconds_with(&lookup), crate::dictate::SR)
                    .map(|config| {
                        let backend: Arc<dyn PreviewBackend> = Arc::new(local.share_for_preview());
                        // Route preview events through the in-process runtime
                        // channel (Codex P1 #608 rust_session_real_backends.rs:372).
                        // The pre-fix wiring passed `stderr_preview_sink()`,
                        // which writes preview events to the process's stderr;
                        // the in-process engine's UI only reads events from
                        // the `RuntimeEvent` channel, so the previews never
                        // surfaced. `runtime_channel_preview_sink` publishes
                        // each preview as a `RuntimeEvent::Worker` whose
                        // payload is byte-equivalent to the parsed shape the
                        // subprocess path produces, keeping the UI's
                        // downstream handling identical across paths.
                        PreviewEngine::spawn(
                            backend,
                            config,
                            runtime_channel_preview_sink(tx.clone(), repaint_notifier.clone()),
                        )
                    })
            }
            ProductionTranscribeBackend::Cloud(_) => None,
        };

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
        // without an app restart. ConfigFirst, matching the reloading prompt on
        // the backend above.
        // Provenance banner: name the resolved stack ONCE so the
        // diagnostic log answers "which code path serves my dictation"
        // without having to correlate a `[runtime] Phase B ...` line
        // against a `[ui] starting: python.exe ...` line and guess.
        // `stt_impl` comes from the backend we just CONSTRUCTED, so it
        // cannot disagree with what runs; `accel` is the plan (see
        // `startup_provenance_line`) stamped from the GPU policy here.
        let session_config = session_config_with(&lookup);
        if matches!(transcribe, ProductionTranscribeBackend::Local(_)) {
            // Only meaningful on the local path: the GPU policy governs
            // whisper.cpp, and a cloud session has no local model to plan
            // for (its banner reports `accel=unknown`).
            //
            // This CLEARS any previous session's observation as well as
            // stamping the plan: this session's model is not loaded yet,
            // so a second session in the same process (retried install,
            // policy flip) must not inherit the old verdict as its banner.
            // Codex P2 #687 round 2.
            let policy = crate::whisper::gpu::parse_gpu_policy(
                runtime.value(crate::whisper::GPU_ENV),
                Some(&settings.device),
            )
            .unwrap_or_default();
            crate::whisper::accel::begin_session(policy);
        }
        let (stt_impl, stt_accel) = startup_provenance_for(&transcribe);
        crate::diag::log!(
            "{}",
            startup_provenance_line(&session_config, stt_impl, stt_accel)
        );

        let mut dictate = DictateSession::new(transcribe, inject, session_config)
            .with_worker_events_enabled()
            .with_owned_command_hook_activity(runtime_active)
            .with_reloading_dictionary_settings(dictionary_settings)
            // Audible PTT press/release cues -- parity with the Python
            // engine's `vp_feedback.play_cue`. The sink itself reads
            // `VOICEPI_FEEDBACK_SOUNDS` live on every call, so the
            // operator's `VOICEPI_FEEDBACK_SOUNDS=1` opt-in works the
            // same way it does on the Python engine (env / config.json
            // overlay, live reload). Closes parity blocker #3.
            .with_cue_sink(Box::new(crate::dictate::SessionCueSink::new(
                settings.feedback_sounds,
            )))
            // JSONL history sink parity with the Python engine — every
            // successful utterance lands in the same local history file
            // `vp_history` writes to today. `history_sink_from_settings`
            // returns `None` when `history_enabled=false`, so this pays
            // zero per-utterance cost on that path. Closes parity blocker #1.
            .with_optional_history_sink(crate::dictate::history_sink_from_app_settings(settings))
            // JSONL metrics sink parity (blocker #6).
            .with_optional_metrics_sink(crate::dictate::metrics_sink_from_app_settings(settings))
            // Live partial-transcription preview parity (blocker #4).
            .with_optional_preview_engine(preview_engine)
            // Audio ducking parity (blocker #2). `SystemAudioDucker::from_env`
            // reads `VOICEPI_AUDIO_DUCKING` + `VOICEPI_AUDIO_DUCKING_LEVEL`
            // and early-returns without touching WASAPI when the gate is off.
            .with_ducker(Box::new(crate::dictate::SystemAudioDucker::new(
                settings.audio_ducking,
                settings.audio_ducking_level.parse().unwrap_or(0.25),
            )))
            // Per-utterance target-window profile matcher (Codex P1 #607).
            // Previously never attached in production, so users' `apply_profile`
            // config was dead code -- Settings changes never fired on the Rust
            // engine. `ReloadingProfileMatcher` re-reads `config.json` on every
            // press (matching Python's `_reload_live_config_if_changed`);
            // `SystemForegroundWindow` is the per-OS focused-window probe.
            .with_profile_matcher(
                Box::new(
                    crate::dictate::profile::ReloadingProfileMatcher::with_config_path(
                        config_path.map(std::path::Path::to_path_buf),
                    ),
                ),
                Box::new(crate::platform::foreground_window::SystemForegroundWindow),
            );
        // Codex P1 #607: always attach the post-processing pass so a profile
        // that flips `post_processor=ollama` (Python parity) reaches an
        // actual backend. `PostProcessBackend::is_active` gates the pass on
        // Python's `processor != "none" && mode != "raw"` -- a stock
        // (unset) config still emits no `post-processing` status and pays
        // zero per-utterance cost.
        dictate = dictate.with_post_process(Box::new(
            crate::postprocess::SessionPostProcess::from_settings(
                crate::postprocess::settings_from_env_with(&lookup),
            ),
        ));
        let session: Arc<Mutex<RealSession>> = Arc::new(Mutex::new(dictate));

        // Spawn the audio pump LAST so a model-path / idle-timeout
        // parse failure does not leak the cpal stream. Pump construction
        // itself is fail-fast: the terminal caller surfaces initialization
        // errors, while the tray supervisor may explicitly fall back to its
        // diagnostic stub path.
        let audio = super::rust_session_audio::AudioPump::spawn_for_session_with_device(
            Arc::clone(&session),
            tx,
            repaint_notifier,
            &settings.audio_device,
        )
        .map_err(|e| format!("audio pump: {e:#}"))?;

        let capture_stop = audio.capture_stop();
        Ok(RealSessionDeps {
            session,
            capture_stop,
            audio,
        })
    }
}

#[cfg(test)]
#[path = "rust_session_real_backends_tests.rs"]
mod tests;
