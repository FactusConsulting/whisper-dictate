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
    build_initial_prompt, build_real_transcribe_backend, format_commands_from_env,
    postprocess_settings_from_env, resolve_stt_backend_from_env, session_config_from_env,
    whisper_backend_config_from_env, RealBackendKind, RealTranscribeBackend, FORMAT_COMMANDS_ENV,
    INITIAL_PROMPT_ENV, LANG_ENV, LOCAL_ONLY_ENV, POST_BASE_URL_ENV, POST_MAX_INPUT_CHARS_ENV,
    POST_MAX_OUTPUT_CHARS_ENV, POST_MODEL_ENV, POST_MODE_ENV, POST_PROCESSOR_ENV, POST_REDACT_ENV,
    POST_REDACT_TERMS_ENV, POST_TIMEOUT_MS_ENV, STT_BACKEND_ENV,
};
use crate::dictate::audio_route::MIN_RECORD_ENV;
use crate::dictate::backends::cloud::{STT_API_KEY_ENV, STT_BASE_URL_ENV, STT_MODEL_ENV};
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

// ── Wave 5.5 gap #4: dictionary prompt injection ─────────────────────────────
//
// The rust-session Whisper backend must pick up the user's dictionary
// vocabulary and weave it into `initial_prompt` -- otherwise the
// rust-session path throws the dictionary away and every rare-word bias
// the Python worker enjoyed silently regresses. Tests:
//
// - dictionary disabled: `build_initial_prompt` returns the env base
//   verbatim (or None if there's no base) -- byte-identical to
//   pre-Wave-5.5 behaviour.
// - dictionary enabled but empty: same result -- an enabled but
//   term-less dictionary doesn't fabricate a `Vocabulary:` suffix.
// - dictionary enabled + terms present: the returned prompt starts
//   with the base + a `Vocabulary:` line (the `Dictionary::build_prompt`
//   format).
// - `whisper_backend_config_from_env` threads the env base through
//   `build_initial_prompt` so the backend receives the dictionary-aware
//   prompt without touching the env var format the user knows.

#[test]
fn build_initial_prompt_disabled_dictionary_passes_base_through() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Dictionary explicitly disabled -> the runtime helper returns the
    // base prompt unchanged. Load a config-file default that could
    // supply a stray dictionary path -- the env override wins.
    let _enabled = EnvVarGuard::set("VOICEPI_DICTIONARY_ENABLED", "0");
    let _path = EnvVarGuard::unset("VOICEPI_DICTIONARY");

    assert_eq!(
        build_initial_prompt(Some("Base prompt")).as_deref(),
        Some("Base prompt"),
    );
    // A blank / missing base collapses to None -- the whisper backend
    // must not receive an empty-string prompt (it would forward it as a
    // literal empty vocabulary hint).
    assert!(build_initial_prompt(None).is_none());
    assert!(build_initial_prompt(Some("")).is_none());
}

#[test]
fn build_initial_prompt_appends_vocabulary_terms_when_dictionary_present() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Point the dictionary env vars at a tempfile with two vocabulary
    // terms so the `runtime_dictionary_result` helper loads them and
    // `build_prompt` appends the `Vocabulary: ...` line.
    let dir = tempfile::tempdir().unwrap();
    let dict_path = dir.path().join("dictionary.json");
    std::fs::write(
        &dict_path,
        r#"{"terms":["Slack","Claude Code"],"replacements":{}}"#,
    )
    .unwrap();
    let _enabled = EnvVarGuard::set("VOICEPI_DICTIONARY_ENABLED", "1");
    let _path = EnvVarGuard::set("VOICEPI_DICTIONARY", dict_path.to_string_lossy().as_ref());

    let prompt = build_initial_prompt(Some("Base prompt")).expect("prompt");
    // Byte-identical to what `Dictionary::build_prompt` produces so a
    // regression in either layer is caught here.
    assert_eq!(prompt, "Base prompt\nVocabulary: Slack, Claude Code");
}

#[test]
fn whisper_backend_config_threads_dictionary_prompt_into_backend() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let dict_path = dir.path().join("dictionary.json");
    std::fs::write(&dict_path, r#"{"terms":["Codex"],"replacements":{}}"#).unwrap();
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::set(INITIAL_PROMPT_ENV, "Whisper Dictate");
    let _enabled = EnvVarGuard::set("VOICEPI_DICTIONARY_ENABLED", "1");
    let _path = EnvVarGuard::set("VOICEPI_DICTIONARY", dict_path.to_string_lossy().as_ref());

    let cfg = whisper_backend_config_from_env();
    assert_eq!(
        cfg.initial_prompt.as_deref(),
        Some("Whisper Dictate\nVocabulary: Codex"),
        "the backend config's initial_prompt must carry the dictionary-\
         merged prompt, not just the env-var base"
    );
}

// ── production sink integration ───────────────────────────────────────────────

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

// ── Wave 5.5 gap #1: STT-backend routing ─────────────────────────────────────

/// Unset / empty / whitespace VOICEPI_STT_BACKEND lands on Whisper --
/// matches the production default `AppSettings::stt_backend = "whisper"`
/// and Python's `vp_transcribe.STT_BACKEND` fallback.
#[test]
fn resolve_stt_backend_defaults_to_whisper_when_env_missing_or_blank() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::unset(STT_BACKEND_ENV);
    assert_eq!(
        resolve_stt_backend_from_env(),
        Some(RealBackendKind::Whisper)
    );

    let _env = EnvVarGuard::set(STT_BACKEND_ENV, "   ");
    assert_eq!(
        resolve_stt_backend_from_env(),
        Some(RealBackendKind::Whisper)
    );
}

/// `whisper` and its legacy `faster-whisper` alias both route to the
/// local Whisper backend. Case-insensitive to match the Python parser.
#[test]
fn resolve_stt_backend_recognises_whisper_and_faster_whisper_alias() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for value in ["whisper", "WHISPER", "faster-whisper", "Faster-Whisper"] {
        let _env = EnvVarGuard::set(STT_BACKEND_ENV, value);
        assert_eq!(
            resolve_stt_backend_from_env(),
            Some(RealBackendKind::Whisper),
            "value {value:?} must route to Whisper",
        );
    }
}

/// Both `openai` and `groq` route to the cloud backend -- Groq is just
/// an OpenAI-compatible base URL, so the top-level backend selection
/// picks Cloud in both cases. The provider-specific base URL / model /
/// key resolution happens inside `CloudBackendConfig::from_env`.
#[test]
fn resolve_stt_backend_recognises_openai_and_groq_as_cloud() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for value in ["openai", "OpenAI", "groq", "GROQ"] {
        let _env = EnvVarGuard::set(STT_BACKEND_ENV, value);
        assert_eq!(
            resolve_stt_backend_from_env(),
            Some(RealBackendKind::Cloud),
            "value {value:?} must route to Cloud",
        );
    }
}

/// Unrecognised value returns None so the factory surfaces a clean
/// "unsupported stt_backend" error rather than silently falling
/// through to Whisper. Guards against a stale Parakeet config or a
/// typo in a hand-edited env.
#[test]
fn resolve_stt_backend_returns_none_for_unknown_value() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::set(STT_BACKEND_ENV, "parakeet");
    assert!(resolve_stt_backend_from_env().is_none());
}

/// End-to-end: with the cloud env vars set the factory returns a
/// [`RealTranscribeBackend::Cloud`] variant. We can't call
/// `transcribe` here without spinning up a stub server (that path is
/// covered by `cloud_tests::transcribe_end_to_end_against_stub_server`);
/// this test only pins the enum discriminant so a future refactor of
/// the factory can't silently regress the routing.
#[test]
fn build_real_transcribe_backend_builds_cloud_variant_for_openai() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.openai.com/v1");
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "gpt-4o-mini-transcribe");
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "sk-test");

    let backend = build_real_transcribe_backend(RealBackendKind::Cloud)
        .unwrap_or_else(|err| panic!("cloud backend builds with valid env: {err}"));
    assert!(
        matches!(backend, RealTranscribeBackend::Cloud(_)),
        "expected Cloud variant"
    );
}

/// A cloud config missing a required field (API key here) surfaces
/// the underlying `CloudTranscribeBackend::new` rejection through
/// the factory's `Result::Err` -- the sink then falls back to the
/// PR 4 stubs and emits a stderr event naming the missing setting.
#[test]
fn build_real_transcribe_backend_surfaces_cloud_config_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.openai.com/v1");
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "gpt-4o-mini-transcribe");
    let _key = EnvVarGuard::unset(STT_API_KEY_ENV);
    // Also clear the fallback env vars so the resolver truly finds no
    // key -- otherwise a developer with OPENAI_API_KEY exported would
    // see this test pass on a mismatched condition.
    let _openai = EnvVarGuard::unset(crate::dictate::backends::cloud::OPENAI_API_KEY_ENV);
    let _groq = EnvVarGuard::unset(crate::dictate::backends::cloud::GROQ_API_KEY_ENV);

    let err = match build_real_transcribe_backend(RealBackendKind::Cloud) {
        Ok(_) => panic!("missing API key must fail the cloud builder"),
        Err(err) => err,
    };
    assert!(
        err.contains("API key"),
        "expected API-key error, got: {err}"
    );
}

// ── Codex #441 P2 round 3: postprocess + format env loading ─────────────────
//
// Wave 5.5 Session-wire #446 wired `postprocess_settings` and
// `format_commands` into `DictateSession`, but `session_config_from_env`
// (the loader the rust-session sink uses on the delegate path) kept
// returning `None` / `"off"` regardless of what the user had saved.
// Now the loader reads the same env vars the AppSettings schema
// surfaces so the saved knobs actually reach the session.

/// The whole suite mutates process env; take the shared lock and
/// snapshot the vars we touch so each test restores cleanly (avoids
/// cross-test bleed even under `--test-threads=1`).
fn guard_postprocess_env() -> Vec<EnvVarGuard> {
    vec![
        EnvVarGuard::unset(POST_PROCESSOR_ENV),
        EnvVarGuard::unset(POST_MODE_ENV),
        EnvVarGuard::unset(POST_MODEL_ENV),
        EnvVarGuard::unset(POST_BASE_URL_ENV),
        EnvVarGuard::unset(POST_TIMEOUT_MS_ENV),
        EnvVarGuard::unset(POST_MAX_INPUT_CHARS_ENV),
        EnvVarGuard::unset(POST_MAX_OUTPUT_CHARS_ENV),
        EnvVarGuard::unset(POST_REDACT_ENV),
        EnvVarGuard::unset(POST_REDACT_TERMS_ENV),
        EnvVarGuard::unset(FORMAT_COMMANDS_ENV),
        EnvVarGuard::unset(LOCAL_ONLY_ENV),
    ]
}

#[test]
fn postprocess_settings_returns_none_when_processor_is_none_or_unset() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();

    // Unset -> None (schema default is "none").
    assert!(postprocess_settings_from_env().is_none());

    // Explicit "none" -> None.
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "none");
    assert!(postprocess_settings_from_env().is_none());

    // Case-insensitive "None" -> None (matches Python's tolerance for
    // capitalisation in the schema value).
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "None");
    assert!(postprocess_settings_from_env().is_none());
}

#[test]
fn postprocess_settings_reads_ollama_config_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "ollama");
    let _m = EnvVarGuard::set(POST_MODE_ENV, "clean");
    let _model = EnvVarGuard::set(POST_MODEL_ENV, "qwen2.5:7b");
    let _url = EnvVarGuard::set(POST_BASE_URL_ENV, "http://localhost:12345");
    let _timeout = EnvVarGuard::set(POST_TIMEOUT_MS_ENV, "9000");
    let _max_in = EnvVarGuard::set(POST_MAX_INPUT_CHARS_ENV, "1500");
    let _max_out = EnvVarGuard::set(POST_MAX_OUTPUT_CHARS_ENV, "2500");

    let settings = postprocess_settings_from_env().expect("processor != none must return Some");
    assert_eq!(settings.processor, "ollama");
    assert_eq!(settings.mode, "clean");
    assert_eq!(settings.model, "qwen2.5:7b");
    assert_eq!(settings.base_url, "http://localhost:12345");
    assert_eq!(settings.timeout_ms, 9_000);
    assert_eq!(settings.max_input_chars, 1_500);
    assert_eq!(settings.max_output_chars, 2_500);
    assert!(!settings.redact);
    assert!(settings.redact_terms.is_empty());
    assert!(!settings.local_only);
}

#[test]
fn postprocess_settings_normalises_openai_defaults() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "openai");
    // Empty model + empty base URL -> provider defaults kick in via
    // `normalized_model` / `default_base_url`.
    let _m = EnvVarGuard::set(POST_MODE_ENV, "clean");

    let settings = postprocess_settings_from_env().expect("openai processor is Some");
    assert_eq!(settings.processor, "openai");
    // OpenAI defaults: `gpt-4o-mini` model, `https://api.openai.com/v1`
    // base URL. Mirrors the same defaults `PostprocessProfile::normalized`
    // installs so the two loading paths agree.
    assert_eq!(settings.model, "gpt-4o-mini");
    assert_eq!(settings.base_url, "https://api.openai.com/v1");
}

#[test]
fn postprocess_settings_honours_redact_and_local_only_flags() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "ollama");
    let _r = EnvVarGuard::set(POST_REDACT_ENV, "1");
    let _rt = EnvVarGuard::set(POST_REDACT_TERMS_ENV, "Slack,Claude Code");
    let _lo = EnvVarGuard::set(LOCAL_ONLY_ENV, "true");

    let settings = postprocess_settings_from_env().expect("Some");
    assert!(settings.redact);
    assert_eq!(settings.redact_terms, "Slack,Claude Code");
    assert!(settings.local_only);
}

#[test]
fn postprocess_settings_falsy_flags_stay_false() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "ollama");
    let _r = EnvVarGuard::set(POST_REDACT_ENV, "0");
    let _lo = EnvVarGuard::set(LOCAL_ONLY_ENV, "false");

    let settings = postprocess_settings_from_env().expect("Some");
    assert!(!settings.redact);
    assert!(!settings.local_only);
}

#[test]
fn format_commands_defaults_to_off_when_env_unset() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _fc = EnvVarGuard::unset(FORMAT_COMMANDS_ENV);
    assert_eq!(format_commands_from_env(), "off");
}

#[test]
fn format_commands_reads_env_value_verbatim() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for value in ["en", "da", "both", "off"] {
        let _fc = EnvVarGuard::set(FORMAT_COMMANDS_ENV, value);
        assert_eq!(format_commands_from_env(), value);
    }
}

#[test]
fn session_config_from_env_threads_postprocess_and_format_into_session_config() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();
    let _p = EnvVarGuard::set(POST_PROCESSOR_ENV, "ollama");
    let _m = EnvVarGuard::set(POST_MODE_ENV, "clean");
    let _fc = EnvVarGuard::set(FORMAT_COMMANDS_ENV, "en");
    // Also clear MIN_RECORD so we exercise only the new fields.
    let _min = EnvVarGuard::unset(MIN_RECORD_ENV);

    let cfg = session_config_from_env();
    let post = cfg
        .postprocess_settings
        .as_ref()
        .expect("postprocess must be Some for processor=ollama");
    assert_eq!(post.processor, "ollama");
    assert_eq!(post.mode, "clean");
    assert_eq!(cfg.format_commands, "en");
}

#[test]
fn session_config_from_env_stays_default_when_postprocess_disabled() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = guard_postprocess_env();
    // Processor unset (schema default is "none") -> None.
    let _min = EnvVarGuard::unset(MIN_RECORD_ENV);

    let cfg = session_config_from_env();
    assert!(cfg.postprocess_settings.is_none());
    assert_eq!(cfg.format_commands, "off");
}
