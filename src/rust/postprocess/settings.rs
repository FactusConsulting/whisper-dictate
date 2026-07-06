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
///
/// Codex-P2 finding on #439: when `processor == "openai"` and the model
/// was left empty (or inherited the Ollama default `qwen2.5:3b`), swap in
/// the OpenAI post default. Same rewrite the Settings-UI path already
/// runs — without it, a minimal `{"processor":"openai"}` profile would
/// call the OpenAI Chat API with an Ollama model id and fall back.
pub fn normalized_model(processor: &str, raw_model: &str) -> String {
    if processor == "groq" && (raw_model.is_empty() || raw_model == DEFAULT_OLLAMA_POST_MODEL) {
        return "llama-3.1-8b-instant".to_owned();
    }
    if processor == "openai" && (raw_model.is_empty() || raw_model == DEFAULT_OLLAMA_POST_MODEL) {
        return "gpt-4o-mini".to_owned();
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
    use super::*;

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
        // Codex-P2 finding on #439: openai now substitutes gpt-4o-mini for
        // both the empty case and the stale Ollama-default case, matching
        // the Groq treatment above and the Settings-UI rewrite.
        assert_eq!(normalized_model("openai", ""), "gpt-4o-mini");
        assert_eq!(
            normalized_model("openai", DEFAULT_OLLAMA_POST_MODEL),
            "gpt-4o-mini"
        );
        assert_eq!(normalized_model("openai", "gpt-4o"), "gpt-4o");
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
