//! Configuration types + validators for the post-processor.
//!
//! Mirrors the Python `PostprocessSettings` + `_default_base_url` /
//! `_normalized_model` / `_normalized_base_url` / `validate_postprocess_settings`
//! helpers so the Rust port accepts exactly the same shapes the Python module
//! ships over the JSON envelope.

use serde::{Deserialize, Serialize};

use crate::cloud_api::{DEFAULT_OPENAI_BASE_URL, GROQ_BASE_URL};
use crate::postprocess::prompt::normalize_mode;
use crate::privacy;

pub const DEFAULT_OLLAMA_POST_MODEL: &str = "qwen2.5:3b";
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
pub const VALID_PROCESSORS: &[&str] = &["none", "ollama", "openai", "groq"];
pub const VALID_MODES: &[&str] = &[
    "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
];

/// Settings shipped from Python (or from a local caller). Field defaults
/// match the Python defaults so a partial JSON payload still produces a
/// usable settings struct.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostprocessSettings {
    #[serde(default = "default_processor")]
    pub processor: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url_str")]
    pub base_url: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_chars")]
    pub max_input_chars: usize,
    #[serde(default = "default_max_chars")]
    pub max_output_chars: usize,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub redact: bool,
    #[serde(default)]
    pub redact_terms: String,
    #[serde(default)]
    pub local_only: bool,
}

fn default_processor() -> String {
    "none".to_owned()
}
fn default_mode() -> String {
    "raw".to_owned()
}
fn default_model() -> String {
    DEFAULT_OLLAMA_POST_MODEL.to_owned()
}
fn default_base_url_str() -> String {
    DEFAULT_OLLAMA_BASE_URL.to_owned()
}
fn default_timeout_ms() -> u64 {
    4_000
}
fn default_max_chars() -> usize {
    4_000
}

// ── env-var sourcing (in-process Rust engine) ────────────────────────────────
//
// The Python worker builds `PostprocessSettings` from config + the keyring
// and ships it to the Rust `postprocess` verb as a JSON envelope. The
// in-process Rust engine (`VOICEPI_DICTATE_ENGINE=rust`) has no Python, so it
// sources the same settings from the `VOICEPI_POST_*` process env the UI's
// worker command already exports (`config::worker_env_overrides` +
// `ui/app.rs`'s API-key push). Field-for-field mirror of `settings_schema.json`.

/// `settings_schema.json` env keys for the post-processor. Kept as named
/// consts so the parser and its tests reference one source of truth.
pub const POST_PROCESSOR_ENV: &str = "VOICEPI_POST_PROCESSOR";
pub const POST_MODE_ENV: &str = "VOICEPI_POST_MODE";
pub const POST_MODEL_ENV: &str = "VOICEPI_POST_MODEL";
pub const POST_BASE_URL_ENV: &str = "VOICEPI_POST_BASE_URL";
pub const POST_TIMEOUT_MS_ENV: &str = "VOICEPI_POST_TIMEOUT_MS";
pub const POST_MAX_INPUT_CHARS_ENV: &str = "VOICEPI_POST_MAX_INPUT_CHARS";
pub const POST_MAX_OUTPUT_CHARS_ENV: &str = "VOICEPI_POST_MAX_OUTPUT_CHARS";
pub const POST_REDACT_ENV: &str = "VOICEPI_POST_REDACT";
pub const POST_REDACT_TERMS_ENV: &str = "VOICEPI_POST_REDACT_TERMS";
/// Shared local-only privacy gate (`settings_schema.json` `local_only`).
pub const LOCAL_ONLY_ENV: &str = "VOICEPI_LOCAL_ONLY";

/// Shared API-key env vars checked before any provider-specific key,
/// highest precedence first: the post-specific override, then the
/// STT-shared key the UI mirrors into the worker env (`ui/app.rs`).
const API_KEY_SHARED_ENV: &[&str] = &["VOICEPI_POST_API_KEY", "VOICEPI_STT_API_KEY"];

/// Build [`PostprocessSettings`] from the process environment. Convenience
/// wrapper around [`settings_from_env_with`] for production callers.
pub fn settings_from_env() -> PostprocessSettings {
    settings_from_env_with(|name| std::env::var(name).ok())
}

/// Parse a numeric post setting with Python `_int_setting` parity:
/// `max(minimum, int(float(value)))`. Accepts decimal forms (`"100.0"`),
/// truncates toward zero, clamps up to `minimum`, and falls back to
/// `default` on unset / blank / unparseable input. Mirrors
/// `vp_postprocess._int_setting` so a below-minimum value (e.g.
/// `VOICEPI_POST_MAX_INPUT_CHARS=0`) can never starve the prompt.
fn int_setting(raw: Option<String>, default: u64, minimum: u64) -> u64 {
    match raw {
        None => default.max(minimum),
        Some(v) => match v.parse::<f64>() {
            Ok(f) if f.is_finite() => (f.trunc().max(0.0) as u64).max(minimum),
            _ => default,
        },
    }
}

/// Testable core of [`settings_from_env`]: resolves every field through the
/// caller-supplied `lookup` so tests can drive it hermetically without
/// touching process env. Empty / whitespace-only values fall back to the
/// same defaults `PostprocessSettings` uses for a missing JSON field, and
/// `model` / `base_url` go through the same `normalized_*` substitution the
/// Python path applies so a saved Ollama default is swapped for the right
/// cloud default when the processor is `openai` / `groq`.
pub fn settings_from_env_with(lookup: impl Fn(&str) -> Option<String>) -> PostprocessSettings {
    let get = |name: &str| {
        lookup(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };

    // Processor: lowercase + reject unknown values (fall back to `none`),
    // mirroring `vp_postprocess.load_postprocess_settings`.
    let mut processor = get(POST_PROCESSOR_ENV)
        .map(|v| v.to_lowercase())
        .unwrap_or_else(default_processor);
    if !VALID_PROCESSORS.contains(&processor.as_str()) {
        processor = default_processor();
    }

    // Mode: normalise aliases (e.g. `bullet-list` -> `bullets`) then reject
    // unknown values, matching the Python loader.
    let mut mode = normalize_mode(&get(POST_MODE_ENV).unwrap_or_else(default_mode));
    if !VALID_MODES.contains(&mode.as_str()) {
        mode = default_mode();
    }

    let raw_model = get(POST_MODEL_ENV).unwrap_or_default();
    // base_url defaults to the *provider's* default (not always Ollama) and
    // has trailing slashes stripped BEFORE normalisation, matching Python's
    // `.rstrip("/")`. Without the strip, `http://localhost:11434/` would not
    // match the Ollama default and a groq/openai processor would send the
    // request to the wrong host instead of substituting the cloud default.
    let raw_base_url = get(POST_BASE_URL_ENV)
        .unwrap_or_else(|| default_base_url(&processor).to_owned())
        .trim_end_matches('/')
        .to_owned();

    // API key: post-specific override, then the STT-shared key, then ONLY
    // the generic env var for the SELECTED provider -- so a groq processor
    // never picks up an `OPENAI_API_KEY` (and vice versa). Mirrors
    // `ui/api_keys.rs::load_post_api_key_from_env`.
    let provider_generic: &[&str] = match processor.as_str() {
        "groq" => &["GROQ_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        _ => &[],
    };
    let api_key = API_KEY_SHARED_ENV
        .iter()
        .chain(provider_generic.iter())
        .find_map(|name| get(name))
        .unwrap_or_default();

    PostprocessSettings {
        model: normalized_model(&processor, &raw_model),
        base_url: normalized_base_url(&processor, &raw_base_url),
        mode,
        timeout_ms: int_setting(get(POST_TIMEOUT_MS_ENV), default_timeout_ms(), 100),
        max_input_chars: int_setting(
            get(POST_MAX_INPUT_CHARS_ENV),
            default_max_chars() as u64,
            100,
        ) as usize,
        max_output_chars: int_setting(
            get(POST_MAX_OUTPUT_CHARS_ENV),
            default_max_chars() as u64,
            100,
        ) as usize,
        redact: crate::dictate::is_truthy(lookup(POST_REDACT_ENV).as_deref()),
        redact_terms: get(POST_REDACT_TERMS_ENV).unwrap_or_default(),
        local_only: crate::dictate::is_truthy(lookup(LOCAL_ONLY_ENV).as_deref()),
        api_key,
        processor,
    }
}

/// Cloud-provider default base URL for the configured processor.
pub fn default_base_url(processor: &str) -> &'static str {
    match processor {
        "groq" => GROQ_BASE_URL,
        "openai" => DEFAULT_OPENAI_BASE_URL,
        _ => DEFAULT_OLLAMA_BASE_URL,
    }
}

/// Pick the right cloud model when the saved settings still hold the local
/// Ollama default. Matches the Python `_normalized_model`.
pub fn normalized_model(processor: &str, raw_model: &str) -> String {
    if processor == "groq" && (raw_model.is_empty() || raw_model == DEFAULT_OLLAMA_POST_MODEL) {
        return "llama-3.1-8b-instant".to_owned();
    }
    if raw_model.is_empty() {
        return DEFAULT_OLLAMA_POST_MODEL.to_owned();
    }
    raw_model.to_owned()
}

/// Match Python `_normalized_base_url`: substitute the right default base URL
/// when the saved value still points at a different processor's default.
pub fn normalized_base_url(processor: &str, raw_base_url: &str) -> String {
    match processor {
        "groq"
            if matches!(
                raw_base_url,
                "" | DEFAULT_OLLAMA_BASE_URL | DEFAULT_OPENAI_BASE_URL
            ) =>
        {
            GROQ_BASE_URL.to_owned()
        }
        "openai" if matches!(raw_base_url, "" | DEFAULT_OLLAMA_BASE_URL | GROQ_BASE_URL) => {
            DEFAULT_OPENAI_BASE_URL.to_owned()
        }
        "ollama" if matches!(raw_base_url, "" | DEFAULT_OPENAI_BASE_URL | GROQ_BASE_URL) => {
            DEFAULT_OLLAMA_BASE_URL.to_owned()
        }
        _ => raw_base_url.to_owned(),
    }
}

/// Validate the settings + apply the local-only gate. Returns `Err(message)`
/// describing the first failure, so the caller can record it in the
/// `error` field of the `PostprocessResult` fallback.
pub fn validate(settings: &PostprocessSettings) -> Result<String, String> {
    let mode = normalize_mode(&settings.mode);
    if !VALID_PROCESSORS.contains(&settings.processor.as_str()) {
        return Err(format!("invalid post processor: {}", settings.processor));
    }
    if !VALID_MODES.contains(&mode.as_str()) {
        return Err(format!("invalid post mode: {}", settings.mode));
    }
    privacy::assert_local_processor(settings.local_only, &settings.processor)
        .map_err(|err| err.to_string())?;
    if !looks_like_http_url(&settings.base_url) {
        return Err(format!(
            "invalid post-process base URL: {:?}",
            settings.base_url
        ));
    }
    if settings.local_only && !privacy::is_loopback_url(&settings.base_url) {
        return Err(format!(
            "VOICEPI_LOCAL_ONLY=1 blocks remote post-processing URL {:?}; use localhost or disable local-only mode.",
            settings.base_url
        ));
    }
    Ok(mode)
}

/// Very small "looks like an HTTP(S) URL with a host" check. Mirrors the
/// pragmatic parser the Python module uses (`urlparse(url).netloc` non-empty)
/// without pulling in a full URL crate just for the validator.
pub fn looks_like_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && url
            .split_once("://")
            .is_some_and(|(_, rest)| !rest.split('/').next().unwrap_or("").trim().is_empty())
}

#[cfg(test)]
mod tests {
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
            redact: false,
            redact_terms: String::new(),
            local_only: false,
        }
    }
}
