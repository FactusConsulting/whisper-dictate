//! Unit tests for [`super`] — post-processor settings + validators.
//!
//! Separate file per the repo convention (`*_tests.rs` alongside the module),
//! which the AGENTS.md test-discipline scanner also looks for.

use std::collections::HashMap;

use super::*;

/// Build a `lookup` closure over a fixed `(env, value)` map for the
/// hermetic `settings_from_env_with` tests.
fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

fn sample_settings(processor: &str, mode: &str, base_url: &str) -> PostprocessSettings {
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
fn settings_from_env_uses_defaults_when_unset() {
    let s = settings_from_env_with(lookup_from(&[]));
    assert_eq!(s.processor, "none");
    assert_eq!(s.mode, "raw");
    assert_eq!(s.model, DEFAULT_OLLAMA_POST_MODEL);
    assert_eq!(s.base_url, DEFAULT_OLLAMA_BASE_URL);
    assert_eq!(s.timeout_ms, 4000);
    assert_eq!(s.max_input_chars, 4000);
    assert_eq!(s.max_output_chars, 4000);
    assert!(!s.redact);
    assert!(!s.local_only);
    assert!(s.api_key.is_empty());
}

#[test]
fn settings_from_env_reads_and_normalizes_fields() {
    // groq processor with the saved Ollama model/base_url defaults ->
    // normalized to the groq cloud defaults (parity with Python).
    let s = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "Groq"), // case-insensitive
        (POST_MODE_ENV, "clean"),
        (POST_MODEL_ENV, DEFAULT_OLLAMA_POST_MODEL),
        (POST_BASE_URL_ENV, DEFAULT_OLLAMA_BASE_URL),
        (POST_TIMEOUT_MS_ENV, "9000"),
        (POST_MAX_INPUT_CHARS_ENV, "2500"),
        (POST_MAX_OUTPUT_CHARS_ENV, "1200"),
        (POST_REDACT_ENV, "1"),
        (POST_REDACT_TERMS_ENV, "Codex, Falcon"),
        (LOCAL_ONLY_ENV, "0"),
    ]));
    assert_eq!(s.processor, "groq");
    assert_eq!(s.mode, "clean");
    assert_eq!(s.model, "llama-3.1-8b-instant");
    assert_eq!(s.base_url, GROQ_BASE_URL);
    assert_eq!(s.timeout_ms, 9000);
    assert_eq!(s.max_input_chars, 2500);
    assert_eq!(s.max_output_chars, 1200);
    assert!(s.redact);
    assert_eq!(s.redact_terms, "Codex, Falcon");
    assert!(!s.local_only);
}

#[test]
fn settings_from_env_api_key_precedence() {
    // Post-specific override wins over the STT-shared and provider keys.
    let s = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "openai"),
        ("VOICEPI_POST_API_KEY", "post-key"),
        ("VOICEPI_STT_API_KEY", "stt-key"),
        ("OPENAI_API_KEY", "openai-key"),
    ]));
    assert_eq!(s.api_key, "post-key");

    // Falls through to the STT-shared key before any provider generic.
    let s = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "groq"),
        ("VOICEPI_STT_API_KEY", "stt-key"),
        ("GROQ_API_KEY", "groq-key"),
    ]));
    assert_eq!(s.api_key, "stt-key");
}

#[test]
fn settings_from_env_generic_api_key_is_provider_aware() {
    // With BOTH generic keys present, a groq processor must read
    // GROQ_API_KEY (not the OpenAI key), and vice versa -- the failure
    // Codex flagged with a single global precedence list.
    let groq = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "groq"),
        ("OPENAI_API_KEY", "openai-key"),
        ("GROQ_API_KEY", "groq-key"),
    ]));
    assert_eq!(groq.api_key, "groq-key");
    let openai = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "openai"),
        ("OPENAI_API_KEY", "openai-key"),
        ("GROQ_API_KEY", "groq-key"),
    ]));
    assert_eq!(openai.api_key, "openai-key");
}

#[test]
fn settings_from_env_strips_trailing_slash_before_normalizing() {
    // A groq processor whose base_url still holds the Ollama default
    // WITH a trailing slash must normalise to the groq cloud endpoint
    // (parity with Python's `.rstrip("/")` before substitution).
    let s = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "groq"),
        (POST_BASE_URL_ENV, "http://localhost:11434/"),
    ]));
    assert_eq!(s.base_url, GROQ_BASE_URL);
}

#[test]
fn settings_from_env_clamps_and_parses_numeric_settings() {
    let s = settings_from_env_with(lookup_from(&[
        (POST_MAX_INPUT_CHARS_ENV, "0"),       // below min -> clamp to 100
        (POST_MAX_OUTPUT_CHARS_ENV, "100.0"),  // decimal -> int(float())
        (POST_TIMEOUT_MS_ENV, "not-a-number"), // unparseable -> default
    ]));
    assert_eq!(s.max_input_chars, 100);
    assert_eq!(s.max_output_chars, 100);
    assert_eq!(s.timeout_ms, 4000);
}

#[test]
fn settings_from_env_blank_values_fall_back_to_defaults() {
    // Whitespace-only env values must not override the defaults nor
    // parse into a zero timeout.
    let s = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "   "),
        (POST_TIMEOUT_MS_ENV, "  "),
        (POST_MODE_ENV, ""),
    ]));
    assert_eq!(s.processor, "none");
    assert_eq!(s.mode, "raw");
    assert_eq!(s.timeout_ms, 4000);
}

#[test]
fn default_base_url_for_processor() {
    assert_eq!(default_base_url("groq"), GROQ_BASE_URL);
    assert_eq!(default_base_url("openai"), DEFAULT_OPENAI_BASE_URL);
    assert_eq!(default_base_url("ollama"), DEFAULT_OLLAMA_BASE_URL);
    assert_eq!(default_base_url("none"), DEFAULT_OLLAMA_BASE_URL);
}

#[test]
fn normalized_model_substitutes_groq_default() {
    assert_eq!(normalized_model("groq", ""), "llama-3.1-8b-instant");
    assert_eq!(
        normalized_model("groq", DEFAULT_OLLAMA_POST_MODEL),
        "llama-3.1-8b-instant"
    );
    assert_eq!(normalized_model("groq", "custom-model"), "custom-model");
    assert_eq!(normalized_model("openai", ""), DEFAULT_OLLAMA_POST_MODEL);
    assert_eq!(normalized_model("ollama", "qwen2.5:14b"), "qwen2.5:14b");
}

#[test]
fn normalized_base_url_substitutes_processor_defaults() {
    assert_eq!(normalized_base_url("groq", ""), GROQ_BASE_URL);
    assert_eq!(
        normalized_base_url("groq", DEFAULT_OLLAMA_BASE_URL),
        GROQ_BASE_URL
    );
    assert_eq!(
        normalized_base_url("openai", DEFAULT_OLLAMA_BASE_URL),
        DEFAULT_OPENAI_BASE_URL
    );
    assert_eq!(
        normalized_base_url("ollama", DEFAULT_OPENAI_BASE_URL),
        DEFAULT_OLLAMA_BASE_URL
    );
    assert_eq!(
        normalized_base_url("openai", "https://api.example.test/v1"),
        "https://api.example.test/v1"
    );
}

#[test]
fn http_url_validator_rejects_missing_host_and_scheme() {
    assert!(looks_like_http_url("http://localhost:11434"));
    assert!(looks_like_http_url("https://api.openai.com/v1"));
    assert!(!looks_like_http_url("ftp://example.com"));
    assert!(!looks_like_http_url("not a url"));
    assert!(!looks_like_http_url("http:///path"));
}

#[test]
fn validate_rejects_invalid_processor() {
    let settings = sample_settings("bogus", "clean", "http://127.0.0.1:1");
    assert!(validate(&settings)
        .unwrap_err()
        .contains("invalid post processor"));
}

#[test]
fn validate_rejects_invalid_mode() {
    let settings = sample_settings("ollama", "garbage", "http://127.0.0.1:1");
    assert!(validate(&settings)
        .unwrap_err()
        .contains("invalid post mode"));
}

#[test]
fn validate_local_only_blocks_remote_url_for_ollama() {
    let mut settings = sample_settings("ollama", "clean", "https://example.com");
    settings.local_only = true;
    assert!(validate(&settings)
        .unwrap_err()
        .contains("VOICEPI_LOCAL_ONLY=1"));
}

#[test]
fn validate_local_only_blocks_openai_even_on_loopback() {
    let mut settings = sample_settings("openai", "clean", "http://localhost:11434");
    settings.local_only = true;
    let err = validate(&settings).unwrap_err();
    assert!(err.contains("VOICEPI_LOCAL_ONLY=1"));
}

#[test]
fn settings_from_env_reads_api_key_endpoint_marker() {
    // Codex P1 #642: the launcher stamps the endpoint the injected key
    // was resolved for, and the pipeline reads it here so the leak check
    // in `run.rs` can compare provider against the current base_url.
    let s = settings_from_env_with(lookup_from(&[
        (POST_PROCESSOR_ENV, "groq"),
        ("VOICEPI_POST_API_KEY", "groq-key"),
        (POST_API_KEY_ENDPOINT_ENV, "https://api.groq.com/openai/v1"),
    ]));
    assert_eq!(s.api_key, "groq-key");
    assert_eq!(s.api_key_endpoint, "https://api.groq.com/openai/v1");
}

#[test]
fn settings_from_env_marker_defaults_to_empty_when_unset() {
    // Backward compat: nothing exported => empty marker => the pipeline
    // never blocks a key the user set themselves.
    let s = settings_from_env_with(lookup_from(&[]));
    assert_eq!(s.api_key_endpoint, "");
}
