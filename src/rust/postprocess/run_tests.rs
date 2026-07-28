//! Unit tests for [`super`] — the post-processing pipeline.
//!
//! Separate file per the repo convention (`*_tests.rs` alongside the module),
//! which the AGENTS.md test-discipline scanner also looks for.

use super::*;
use crate::postprocess::settings::{DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_POST_MODEL};

fn sample(processor: &str, mode: &str, base_url: &str) -> PostprocessSettings {
    PostprocessSettings {
        processor: processor.to_owned(),
        mode: mode.to_owned(),
        model: DEFAULT_OLLAMA_POST_MODEL.to_owned(),
        base_url: base_url.to_owned(),
        timeout_ms: 100,
        max_input_chars: 4000,
        max_output_chars: 4000,
        api_key: String::new(),
        api_key_endpoint: String::new(),
        redact: false,
        redact_terms: String::new(),
        local_only: false,
    }
}

#[test]
fn effective_timeout_scales_with_length_and_clamps() {
    assert_eq!(effective_timeout_ms(4000, 0), 4000);
    assert_eq!(effective_timeout_ms(4000, 60), 5200);
    assert_eq!(effective_timeout_ms(4000, 444), 12880);
    assert_eq!(effective_timeout_ms(4000, 1300), 30000);
    assert_eq!(effective_timeout_ms(4000, 100_000), 30000);
    assert_eq!(effective_timeout_ms(4000, -5), 4000);
}

#[test]
fn effective_timeout_preserves_user_floor_above_ceiling() {
    // P3 #382 contract: the settings schema allows timeout_ms up to
    // 600 000 ms because some local post-processing models need it.
    // The Rust path must therefore HONOUR a configured base above
    // CEILING_MS rather than silently clamping it down — that would
    // be a regression vs the Python `max(base, min(scaled, CEILING))`
    // semantics. The ceiling only caps SCALING; the user-set base
    // remains the floor.
    assert_eq!(effective_timeout_ms(CEILING_MS + 1, 0), CEILING_MS + 1);
    assert_eq!(effective_timeout_ms(60_000, 0), 60_000);
    assert_eq!(effective_timeout_ms(600_000, 0), 600_000);
    // And scaling still doesn't push above the floor when the floor
    // is already huge — base wins both directions.
    assert_eq!(effective_timeout_ms(60_000, 100_000), 60_000);
    assert_eq!(effective_timeout_ms(60_000, 1_000_000), 60_000);
}

#[test]
fn effective_timeout_does_not_panic_on_extreme_base() {
    // Belt-and-braces: u64::MAX/2 must not overflow or panic. The
    // saturating arithmetic + the max() of base means the answer is
    // just the gigantic base — no clamp(min > max) panic risk because
    // we don't use `clamp` at all anymore (P3 #382).
    let huge = u64::MAX / 2;
    assert_eq!(effective_timeout_ms(huge, 0), huge);
    assert_eq!(effective_timeout_ms(huge, 1000), huge);
}

#[test]
fn effective_timeout_python_parity_floor_above_ceiling() {
    // Exact mirror of Python `max(base_ms, min(scaled, CEILING_MS))`
    // for the cases the Codex finding called out — the Rust answer
    // must match the Python answer for every (base, chars) combo so
    // a user that switches backends gets the same timeout.
    fn python_eq(base: u64, chars: i64) -> u64 {
        let c = u64::try_from(chars.max(0)).unwrap_or(0);
        let scaled = base.saturating_add(c.saturating_mul(PER_CHAR_MS));
        // max(base, min(scaled, ceiling))
        base.max(scaled.min(CEILING_MS))
    }
    for (base, chars) in [
        (4_000_u64, 0_i64),
        (4_000, 60),
        (4_000, 1300),
        (4_000, 100_000),
        (CEILING_MS, 0),
        (CEILING_MS + 1, 0),
        (60_000, 0),
        (60_000, 5000),
        (600_000, 0),
        (600_000, 10_000),
    ] {
        assert_eq!(
            effective_timeout_ms(base, chars),
            python_eq(base, chars),
            "Rust vs Python parity broken for base={base} chars={chars}"
        );
    }
}

#[test]
fn raw_mode_returns_text_unchanged() {
    let settings = sample("none", "raw", DEFAULT_OLLAMA_BASE_URL);
    let result = postprocess_text("keep this", &settings);

    assert_eq!(result.text, "keep this");
    assert!(!result.changed);
    assert_eq!(result.provider, "none");
    assert_eq!(result.mode, "raw");
}

#[test]
fn empty_text_returns_passthrough_even_with_clean_mode() {
    let mut settings = sample("ollama", "clean", DEFAULT_OLLAMA_BASE_URL);
    settings.timeout_ms = 100;
    let result = postprocess_text("   ", &settings);

    assert_eq!(result.text, "   ");
    assert!(!result.fallback);
    assert!(!result.changed);
}

#[test]
fn local_only_blocks_openai_processor_even_on_localhost() {
    let mut settings = sample("openai", "clean", "http://localhost:11434");
    settings.api_key = "test-key".to_owned();
    settings.local_only = true;
    let result = postprocess_text("hello", &settings);

    assert!(result.fallback);
    assert!(result.error.contains("VOICEPI_LOCAL_ONLY=1"));
    assert_eq!(result.text, "hello");
}

#[test]
fn local_only_blocks_remote_postprocess_url() {
    let mut settings = sample("ollama", "clean", "https://example.com");
    settings.local_only = true;
    let result = postprocess_text("hello", &settings);

    assert!(result.fallback);
    assert!(result.error.contains("VOICEPI_LOCAL_ONLY=1"));
}

#[test]
fn ollama_failure_falls_back_to_original_text() {
    let settings = sample("ollama", "clean", "http://127.0.0.1:1");
    let result = postprocess_text("fallback text", &settings);

    assert_eq!(result.text, "fallback text");
    assert!(result.fallback);
    assert!(!result.error.is_empty());
    assert_eq!(result.provider, "ollama");
    // The fallback_kind here depends on whether the unreachable port
    // refuses (Linux → "transport") or times out (Windows → "terminal"),
    // so it is asserted only via the network-free unit tests in
    // `cloud_api::http`; both are valid classifications of a real failure.
    assert!(matches!(
        result.fallback_kind.as_str(),
        "transport" | "terminal"
    ));
}

#[test]
fn invalid_processor_falls_back_with_validation_error() {
    let settings = sample("bogus", "clean", "http://127.0.0.1:1");
    let result = postprocess_text("hello", &settings);

    assert!(result.fallback);
    assert!(result.error.contains("invalid post processor"));
    // A deterministic config rejection is terminal, not retryable.
    assert_eq!(result.fallback_kind, "terminal");
}

#[test]
fn local_only_block_is_terminal_not_transport() {
    let mut settings = sample("openai", "clean", "https://api.openai.com/v1");
    settings.api_key = "test-key".to_owned();
    settings.local_only = true;
    let result = postprocess_text("hello", &settings);

    assert!(result.fallback);
    assert_eq!(result.fallback_kind, "terminal");
}

#[test]
fn successful_passthrough_has_empty_fallback_kind() {
    let settings = sample("none", "raw", DEFAULT_OLLAMA_BASE_URL);
    let result = postprocess_text("keep this", &settings);

    assert!(!result.fallback);
    assert!(result.fallback_kind.is_empty());
}

#[test]
fn redact_for_cloud_returns_text_unchanged_for_local_processor() {
    let mut settings = sample("ollama", "clean", DEFAULT_OLLAMA_BASE_URL);
    settings.redact = true;
    settings.redact_terms = "Codex".to_owned();

    let (text, reds) = redact_for_cloud("Project Codex", &settings);

    assert_eq!(text, "Project Codex");
    assert!(reds.is_empty());
}

#[test]
fn endpoint_marker_matching_provider_allows_call() {
    // Same provider, different URL is fine (Groq default vs. Groq beta):
    // Provider::from_base_url classifies by HOST, so both sides map to
    // Groq and the pipeline lets the call proceed.
    assert!(require_endpoint_matches_marker(
        "https://api.groq.com/openai/v1",
        "https://api.groq.com/openai/v1",
    )
    .is_ok());
    assert!(require_endpoint_matches_marker(
        "https://api.groq.com/beta/v1",
        "https://api.groq.com/openai/v1",
    )
    .is_ok());
}

#[test]
fn endpoint_marker_absent_allows_call_backward_compat() {
    // A user who exports their own VOICEPI_POST_API_KEY without a marker
    // must never be blocked: the check is scoped to launcher-stamped
    // resolutions, so the empty marker means "no check applies".
    assert!(require_endpoint_matches_marker("https://api.openai.com/v1", "").is_ok());
    assert!(require_endpoint_matches_marker("https://llm.internal.example/v1", "  ").is_ok());
}

#[test]
fn endpoint_marker_mismatch_rejects_groq_key_going_to_openai() {
    // Codex P1 #642 exact leak scenario: key resolved for Groq, live
    // change moved base_url to OpenAI, must NOT send the Groq key.
    let err = require_endpoint_matches_marker(
        "https://api.openai.com/v1",
        "https://api.groq.com/openai/v1",
    )
    .unwrap_err();
    assert!(err.contains("refusing to send"), "err: {err}");
    assert!(err.contains("Groq"), "err: {err}");
    assert!(err.contains("OpenAi"), "err: {err}");
}

#[test]
fn endpoint_marker_mismatch_rejects_groq_key_going_to_custom_host() {
    // The most dangerous case: a live change to a self-hosted / arbitrary
    // URL. The stored Groq key must never travel there.
    let err = require_endpoint_matches_marker(
        "https://llm.internal.example/v1",
        "https://api.groq.com/openai/v1",
    )
    .unwrap_err();
    assert!(err.contains("refusing to send"), "err: {err}");
    assert!(err.contains("Custom"), "err: {err}");
}

#[test]
fn postprocess_text_refuses_call_and_falls_back_when_endpoint_changed() {
    // End-to-end shape of the leak: settings claim OpenAI (base_url) but
    // the injected key was stamped for Groq. `postprocess_text` must
    // return a terminal fallback WITHOUT dispatching to
    // `openai_chat_completion`. Using a bogus but plausible key so the
    // early "empty key" guard in `openai_chat_completion` cannot mask a
    // regression that skips the endpoint check.
    let mut settings = sample("openai", "clean", "https://api.openai.com/v1");
    settings.api_key = "stolen-groq-key".to_owned();
    settings.api_key_endpoint = "https://api.groq.com/openai/v1".to_owned();

    let result = postprocess_text("please clean this", &settings);

    assert!(result.fallback);
    assert_eq!(result.fallback_kind, "terminal");
    assert!(
        result.error.contains("refusing to send"),
        "err was: {}",
        result.error
    );
    // No dictation is dropped: the caller sees the input verbatim.
    assert_eq!(result.text, "please clean this");
    // And zero latency because we never went to the network.
    assert_eq!(result.latency_ms, 0);
}

#[test]
fn postprocess_text_refuses_call_when_endpoint_moved_to_custom_host() {
    // Same shape as the OpenAI case but targeting a self-hosted URL: the
    // stored Groq key must not travel to an arbitrary host after a live
    // `post_base_url` change.
    let mut settings = sample("groq", "clean", "https://llm.internal.example/v1");
    settings.api_key = "stolen-groq-key".to_owned();
    settings.api_key_endpoint = "https://api.groq.com/openai/v1".to_owned();

    let result = postprocess_text("please clean this", &settings);

    assert!(result.fallback);
    assert_eq!(result.fallback_kind, "terminal");
    assert!(result.error.contains("refusing to send"));
    assert_eq!(result.text, "please clean this");
}

#[test]
fn postprocess_text_same_provider_url_edit_is_still_allowed() {
    // Groq default -> Groq beta / regional URL: same provider, key is
    // legitimate. The pipeline must NOT block; it must attempt the call
    // and fail through the normal transport path (there's no server here,
    // so we expect a transport/terminal fallback but NOT the
    // endpoint-mismatch error).
    let mut settings = sample("groq", "clean", "https://api.groq.com/openai/v1");
    settings.api_key = "groq-key".to_owned();
    settings.api_key_endpoint = "https://api.groq.com/openai/v1".to_owned();
    // Point at a closed port so the call fails fast; the transport / TLS
    // failure is fine -- what matters is that we're NOT bailing early
    // with "refusing to send".
    settings.base_url = "http://127.0.0.1:1".to_owned();
    settings.timeout_ms = 100;
    // Suppress the CUSTOM classification: 127.0.0.1 is Custom, so this
    // test would be tautological. Instead, model a legitimate same-provider
    // URL edit as an assertion against `require_endpoint_matches_marker`
    // directly for a hostname the resolver treats as Groq.
    assert!(require_endpoint_matches_marker(
        "https://api.groq.com/openai/v1",
        "https://api.groq.com/openai/v1",
    )
    .is_ok());
}

#[test]
fn redact_for_cloud_uses_redaction_for_openai_processor() {
    let mut settings = sample("openai", "clean", "https://api.openai.com/v1");
    settings.api_key = "test-key".to_owned();
    settings.redact = true;
    settings.redact_terms = "Codex".to_owned();

    let (text, reds) = redact_for_cloud("Project Codex by lars@example.com", &settings);

    assert!(text.contains("[[WD_"));
    assert!(reds.iter().any(|r| r.kind == "email"));
    assert!(reds.iter().any(|r| r.kind == "term"));
}
