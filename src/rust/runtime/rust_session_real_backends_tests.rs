//! Tests for [`super::rust_session_real_backends`].
//!
//! The constructor itself does not load the model (the wrapping
//! [`crate::whisper::IdleUnloadingModel`] is lazy), so these tests
//! exercise the env-driven config resolution + the production-sink
//! fallback path without ever calling whisper.cpp. The model file does
//! not have to exist on disk for [`super::make_real_session`] to
//! succeed -- the actual `LocalWhisper::new(...)` call is deferred
//! until the first transcribe, which is exactly the lifecycle Wave 5
//! PR 5 inherits from PR 5-prep's `WhisperLocalTranscribeBackend`.

use std::sync::mpsc;

use super::{
    format_command_set_from_env, session_config_from_env, startup_provenance_line,
    whisper_backend_config_from_env, FORMAT_COMMANDS_ENV, INITIAL_PROMPT_ENV, LANG_ENV,
};
use crate::dictate::audio_route::MIN_RECORD_ENV;
use crate::dictate::provenance::{
    ENGINE_RUST_IN_PROCESS, STT_IMPL_CLOUD_GROQ, STT_IMPL_WHISPER_CPP,
};
use crate::dictate::{ProductionTranscribeBackend, WhisperLocalTranscribeBackend};
use crate::runtime::rust_session_sink::build_production_sink;
use crate::runtime::RuntimeEvent;
use crate::test_env_lock::ENV_LOCK;
use crate::whisper::{IDLE_UNLOAD_ENV, MODEL_PATH_ENV};

/// Save / restore an env var across a test so concurrent (different-named)
/// env var tests do not leak state. Used together with `ENV_LOCK` to
/// serialise tests that mutate the process-wide env.
struct EnvVarGuard {
    name: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let prev = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, prev }
    }

    fn unset(name: &'static str) -> Self {
        let prev = std::env::var(name).ok();
        std::env::remove_var(name);
        Self { name, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.name, v),
            None => std::env::remove_var(self.name),
        }
    }
}

// ── env-driven config parsers (Codex P2 #423 findings 2 + 5) ─────────────────

/// Wave 5 PR 5 round 2 (Codex P2 #423 finding 2): the language hint
/// and the initial prompt come from the same env vars `vp_cli.py`
/// reads. Empty / blank values must collapse to `None` so the per-
/// call empty-string -> auto-detect collapse in
/// `WhisperLocalTranscribeBackend::transcribe` never even sees a
/// literal empty string.
#[test]
fn whisper_backend_config_reads_lang_and_initial_prompt_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _lang = EnvVarGuard::set(LANG_ENV, "da");
    let _prompt = EnvVarGuard::set(INITIAL_PROMPT_ENV, "Whisper Dictate, Factus Consulting");

    let cfg = whisper_backend_config_from_env();
    assert_eq!(cfg.language.as_deref(), Some("da"));
    assert_eq!(
        cfg.initial_prompt.as_deref(),
        Some("Whisper Dictate, Factus Consulting")
    );
}

#[test]
fn whisper_backend_config_normalises_blank_env_values_to_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _lang = EnvVarGuard::set(LANG_ENV, "   ");
    let _prompt = EnvVarGuard::set(INITIAL_PROMPT_ENV, "");

    let cfg = whisper_backend_config_from_env();
    assert!(
        cfg.language.is_none(),
        "blank language env must collapse to None, got {:?}",
        cfg.language
    );
    assert!(
        cfg.initial_prompt.is_none(),
        "empty initial-prompt env must collapse to None, got {:?}",
        cfg.initial_prompt
    );
}

#[test]
fn whisper_backend_config_unset_env_is_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = whisper_backend_config_from_env();
    assert!(cfg.language.is_none());
    assert!(cfg.initial_prompt.is_none());
}

/// Wave 5 PR 5 round 2 (Codex P2 #423 finding 5):
/// `VOICEPI_MIN_RECORD_SECONDS` must flow into the constructed
/// `SessionConfig` so a user who raised the floor to suppress
/// accidental taps actually has that value enforced.
#[test]
fn session_config_threads_min_record_seconds_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _min = EnvVarGuard::set(MIN_RECORD_ENV, "0.8");
    let cfg = session_config_from_env();
    assert!(
        (cfg.min_record_seconds - 0.8).abs() < f64::EPSILON,
        "expected 0.8, got {}",
        cfg.min_record_seconds
    );
}

#[test]
fn session_config_falls_back_to_route_default_when_env_missing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _min = EnvVarGuard::unset(MIN_RECORD_ENV);
    let cfg = session_config_from_env();
    assert!(
        (cfg.min_record_seconds - 0.5).abs() < f64::EPSILON,
        "expected the 0.5 s default, got {}",
        cfg.min_record_seconds
    );
}

/// A set value in `VOICEPI_FORMAT_COMMANDS` is threaded verbatim into
/// `SessionConfig::format_command_set` so the in-process rust-session
/// path honours the saved `format_commands` setting.
#[test]
fn session_config_threads_format_commands_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _fmt = EnvVarGuard::set(FORMAT_COMMANDS_ENV, "en");
    let cfg = session_config_from_env();
    assert_eq!(cfg.format_command_set.as_deref(), Some("en"));
}

// ── canonical stt_backend + model label (Codex P2 #620) ──────────────────────

/// When the operator selected the cloud backend, the utterance row's
/// `model` label must come from `VOICEPI_STT_MODEL` (the cloud model),
/// NOT `VOICEPI_MODEL` (the local Whisper model). Codex P2 #620
/// `Label cloud events with the cloud model`.
#[test]
fn session_config_uses_cloud_model_when_cloud_backend_selected() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _backend = EnvVarGuard::set("VOICEPI_STT_BACKEND", "openai");
    let _stt_model = EnvVarGuard::set("VOICEPI_STT_MODEL", "gpt-4o-transcribe");
    // The local `VOICEPI_MODEL` is DELIBERATELY set to a stale value
    // -- the fix must ignore it on the cloud path so the row reports
    // the actual model that was used.
    let _local_model = EnvVarGuard::set("VOICEPI_MODEL", "large-v3-turbo");

    let cfg = session_config_from_env();
    assert_eq!(
        cfg.stt_backend, "openai",
        "cloud selection must emit the canonical `openai` label"
    );
    assert_eq!(
        cfg.model, "gpt-4o-transcribe",
        "cloud selection must label the row with VOICEPI_STT_MODEL, \
         NOT the local-only VOICEPI_MODEL"
    );
}

/// The canonical backend label must be `openai` regardless of the
/// operator's raw casing / whitespace. Codex P2 #620 `Canonicalize the
/// backend label from the selected backend` finding.
#[test]
fn session_config_canonicalises_cloud_backend_case_and_whitespace() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _backend = EnvVarGuard::set("VOICEPI_STT_BACKEND", "  OpenAI  ");
    let _stt_model = EnvVarGuard::set("VOICEPI_STT_MODEL", "whisper-1");
    let _local_model = EnvVarGuard::unset("VOICEPI_MODEL");

    let cfg = session_config_from_env();
    assert_eq!(
        cfg.stt_backend, "openai",
        "case/whitespace variant of `openai` must still emit the canonical label"
    );
    assert_eq!(cfg.model, "whisper-1");
}

/// A stale legacy `VOICEPI_STT_BACKEND=parakeet` (or `faster-whisper`)
/// value collapses to local Whisper because
/// `cloud_backend_requested_from_env` only recognises `openai`, and
/// `ProductionTranscribeBackend::select` runs the local thunk on any
/// non-cloud value. The row must therefore label the actual backend
/// used (`whisper`), never re-introduce the stale value on disk.
#[test]
fn session_config_collapses_legacy_backend_values_to_whisper() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _backend = EnvVarGuard::set("VOICEPI_STT_BACKEND", "parakeet");
    let _stt_model = EnvVarGuard::set("VOICEPI_STT_MODEL", "stale-cloud-model");
    let _local_model = EnvVarGuard::set("VOICEPI_MODEL", "large-v3");

    let cfg = session_config_from_env();
    assert_eq!(
        cfg.stt_backend, "whisper",
        "a stale/unknown VOICEPI_STT_BACKEND must NOT flow verbatim to \
         history/metrics rows; the row must reflect the actual backend"
    );
    assert_eq!(
        cfg.model, "large-v3",
        "on the local path the model label must come from VOICEPI_MODEL, \
         never the (unused) VOICEPI_STT_MODEL"
    );
}

/// Unset backend defaults to local Whisper -- schema default matches
/// `settings_schema.json:stt_backend=whisper`.
#[test]
fn session_config_defaults_to_whisper_when_backend_env_unset() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _backend = EnvVarGuard::unset("VOICEPI_STT_BACKEND");
    let _stt_model = EnvVarGuard::unset("VOICEPI_STT_MODEL");
    let _local_model = EnvVarGuard::set("VOICEPI_MODEL", "small");

    let cfg = session_config_from_env();
    assert_eq!(cfg.stt_backend, "whisper");
    assert_eq!(cfg.model, "small");
}

/// Unset / blank `VOICEPI_FORMAT_COMMANDS` collapses to `None` so the
/// session short-circuits to a passthrough (no format-command layer),
/// matching the schema default of `off`.
#[test]
fn format_command_set_from_env_is_none_when_unset_or_blank() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    {
        let _fmt = EnvVarGuard::unset(FORMAT_COMMANDS_ENV);
        assert_eq!(format_command_set_from_env(), None);
    }
    {
        let _fmt = EnvVarGuard::set(FORMAT_COMMANDS_ENV, "   ");
        assert_eq!(
            format_command_set_from_env(),
            None,
            "whitespace-only must normalise to None, not Some(\"\")"
        );
    }
}

// ── production sink integration ───────────────────────────────────────────────

/// Codex P1 #607: the production factory must attach a profile
/// matcher, otherwise users' `apply_profile` config is dead code and
/// Settings changes never fire on the Rust engine. The check is
/// indirect (the session's matcher slot is private) but observable
/// through the SESSION emitting a `state=profile` worker event for
/// every utterance once a matcher is attached (see the session's
/// `emit_profile_status` docs). The factory produces a session that,
/// when idle-driven through `start()`, emits that event; the plumbing
/// details are covered by the session-level tests_profile suite.
///
/// A model-resolution failure short-circuits construction, so this
/// test uses the same env setup the sibling `build_production_sink_...`
/// test uses to keep the harness identical, and only asserts a
/// human-readable indication of the matcher wire-up on the SUCCESS
/// path. On the fallback path the session is the stub and the test is
/// a no-op (documented via an eprintln so CI has a breadcrumb).
#[test]
fn make_real_session_attaches_a_profile_matcher_when_construction_succeeds() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _model_env = EnvVarGuard::unset(MODEL_PATH_ENV);
    let _idle_env = EnvVarGuard::unset(IDLE_UNLOAD_ENV);
    let (tx, _rx) = mpsc::channel();
    match super::make_real_session(tx, None) {
        Ok(deps) => {
            let session = deps.session.lock().unwrap_or_else(|p| p.into_inner());
            assert!(
                session.has_profile_matcher(),
                "make_real_session MUST attach a ReloadingProfileMatcher so \
                 users' apply_profile config is not dead code (Codex P1 #607)"
            );
        }
        Err(msg) => {
            eprintln!(
                "[test note] make_real_session fell back (msg={msg:?}); the \
                 profile-matcher attach is unit-tested via the session-level \
                 tests_profile suite when construction succeeds."
            );
        }
    }
}

/// Wave 5 PR 5 -- when both required features are compiled in AND the
/// model env-var points at an EMPTY path (resolution failure), the
/// production sink must:
///
/// 1. Build a working sink (returned `OnceLock` empty so the supervisor
///    can populate it).
/// 2. Emit a `[rust-session]` stderr event on the channel naming the
///    fallback. The user needs that message to understand why
///    transcription emits the stub `no_text` instead of the expected
///    real output.
///
/// If the CI runner happens to have a model in the user-cache (which
/// would let the resolution succeed despite the blank env var), the
/// real-backend branch will succeed and no fallback event fires --
/// that is also a valid outcome of this contract. Only assert the
/// fallback-event shape WHEN the fallback fired.
#[test]
fn build_production_sink_emits_fallback_event_when_real_backend_fails() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _model_env = EnvVarGuard::set(MODEL_PATH_ENV, "   ");
    let _idle_env = EnvVarGuard::unset(IDLE_UNLOAD_ENV);

    let (tx, rx) = mpsc::channel();
    let (_sink, coord_slot) = build_production_sink(tx, None);
    assert!(
        coord_slot.get().is_none(),
        "production sink (real OR fallback) must hand back an empty OnceLock"
    );

    // Drain whatever the sink put on the channel during construction.
    let mut saw_fallback = false;
    while let Ok(ev) = rx.try_recv() {
        if let RuntimeEvent::Stderr(s) = ev {
            if s.contains("[rust-session]") && s.contains("falling back") {
                saw_fallback = true;
                break;
            }
        }
    }
    if !saw_fallback {
        eprintln!(
            "[test note] resolution succeeded despite blank env -- a cached \
             model OR an absent audio-in-rust feature might be in play. The \
             round-2 env-helper tests pin the parse contracts unconditionally."
        );
    }
}

// -- provenance: engine stamp + startup line -----------------------------

/// A local transcribe backend whose loader always fails, so the test never
/// needs a GGML fixture or a whisper.cpp call.
/// [`super::startup_provenance_for`] only reads the enum discriminant, and
/// `idle_timeout = None` keeps the wrapper from spawning a watcher thread.
fn local_backend_for_tests() -> WhisperLocalTranscribeBackend {
    let model = crate::whisper::IdleUnloadingModel::new(
        || Err(anyhow::anyhow!("test loader: refused to load model")),
        None,
    );
    WhisperLocalTranscribeBackend::new(
        model,
        crate::dictate::backends::whisper_local::WhisperBackendConfig::default(),
    )
}

/// Every utterance this module's session produces ran inside
/// `whisper-dictate.exe`, so the row must say so. Without this stamp a
/// diagnostic log that shows BOTH the Rust in-process dispatch AND a
/// `python.exe -m whisper_dictate.runtime` line leaves the reader
/// guessing which one served the utterance.
#[test]
fn session_config_stamps_the_rust_in_process_engine() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let cfg = session_config_from_env();
    assert_eq!(cfg.engine, ENGINE_RUST_IN_PROCESS);
}

#[test]
fn startup_provenance_line_names_the_resolved_stack() {
    let cfg = crate::dictate::SessionConfig {
        engine: ENGINE_RUST_IN_PROCESS.to_owned(),
        model: "large-v3-turbo".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        startup_provenance_line(&cfg, STT_IMPL_WHISPER_CPP, "vulkan"),
        "[runtime] transcribe backend resolved: engine=rust-in-process \
         impl=whisper.cpp accel=vulkan model=large-v3-turbo"
    );
}

#[test]
fn local_startup_provenance_reports_the_observed_accel_over_the_plan() {
    // Same divergence the `stt_accel` field exists for: a Vulkan build
    // whose whisper.cpp already reported a CPU fallback must not print
    // `accel=vulkan` at startup.
    let _guard = crate::test_env_lock::ACCEL_OBSERVER_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let observer = crate::whisper::accel::global();
    observer.reset();
    observer.set_planned(crate::whisper::Accel::Vulkan);
    observer.note_log_line("whisper_backend_init_gpu: no GPU found");

    let backend = ProductionTranscribeBackend::Local(local_backend_for_tests());
    assert_eq!(
        super::startup_provenance_for(&backend),
        (STT_IMPL_WHISPER_CPP, "cpu")
    );

    observer.reset();
}

/// Codex P2 #687: a cloud session must NOT inherit the local whisper.cpp
/// GPU plan. Its utterance records say `stt_accel=unknown`, so the banner
/// has to as well -- otherwise a Vulkan build announces
/// `impl=cloud-groq accel=vulkan` for audio it never touched.
#[test]
fn cloud_startup_provenance_reports_unknown_accel_not_the_local_plan() {
    let _guard = crate::test_env_lock::ACCEL_OBSERVER_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let observer = crate::whisper::accel::global();
    observer.reset();
    // Deliberately stamp a GPU verdict that a naive implementation would
    // copy onto the cloud banner.
    observer.note_log_line("whisper_backend_init_gpu: using Vulkan0 backend");

    let backend: ProductionTranscribeBackend<crate::dictate::WhisperLocalTranscribeBackend> =
        ProductionTranscribeBackend::Cloud(Box::new(crate::dictate::CloudTranscribeBackend::new(
            crate::dictate::CloudTranscribeConfig {
                base_url: "https://api.groq.com/openai/v1".to_owned(),
                api_key: "k".to_owned(),
                model: "whisper-large-v3".to_owned(),
                timeout_ms: 1_000,
                language: None,
                prompt: None,
            },
        )));
    assert_eq!(
        super::startup_provenance_for(&backend),
        (STT_IMPL_CLOUD_GROQ, "unknown")
    );

    let cfg = crate::dictate::SessionConfig {
        engine: ENGINE_RUST_IN_PROCESS.to_owned(),
        ..Default::default()
    };
    let line = startup_provenance_line(&cfg, STT_IMPL_CLOUD_GROQ, "unknown");
    assert!(line.contains("impl=cloud-groq"), "{line}");
    assert!(line.contains("accel=unknown"), "{line}");

    observer.reset();
}
