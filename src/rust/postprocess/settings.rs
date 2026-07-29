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
    /// The NORMALISED endpoint the launcher resolved `api_key` for, if any.
    ///
    /// Set by `runtime::cloud_api_keys::attach_cloud_api_keys` when it injects
    /// `VOICEPI_POST_API_KEY` from the credential store, propagated to the
    /// worker as `VOICEPI_POST_API_KEY_ENDPOINT`. The pipeline compares this
    /// marker's provider to `base_url`'s provider on every cloud call and
    /// REFUSES to send the key when they differ, so a live `post_processor`
    /// or `post_base_url` change cannot exfiltrate a stored key to a different
    /// host (Codex P1 #642).
    ///
    /// Empty means "no marker" -- either the user exported their own key or
    /// this is a hermetic test. Backward-compatible: without a marker the
    /// pipeline never blocks, so nothing changes for users who set the key
    /// themselves.
    #[serde(default)]
    pub api_key_endpoint: String,
    #[serde(default)]
    pub redact: bool,
    #[serde(default)]
    pub redact_terms: String,
    #[serde(default)]
    pub local_only: bool,
    /// Configured spoken-language hint (`lang` / `VOICEPI_LANG`), empty when
    /// the user left it on auto-detect.
    ///
    /// Bug #685: the cleanup prompt never mentioned the language, so an LLM
    /// pass in `clean` mode was free to translate the transcript (Danish
    /// "1, 2, 3, 4, 5, 6" came back as English "One, two, three, four, five,
    /// six"). The pipeline now threads this into `prompt::build_prompt`.
    /// `#[serde(default)]` keeps an older Python worker's envelope (which does
    /// not carry the field) deserialising — it just falls back to the
    /// "reply in the same language as the input" wording.
    #[serde(default)]
    pub lang: String,
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
/// Shared spoken-language hint (`settings_schema.json` `lang`). Not a
/// `VOICEPI_POST_*` setting -- the post-processor reads the SAME language the
/// STT pass used so the cleanup prompt can forbid a translation (#685).
pub const LANG_ENV: &str = "VOICEPI_LANG";
/// Marker stamped by `runtime::cloud_api_keys` recording the endpoint the
/// injected `VOICEPI_POST_API_KEY` was resolved for. Consulted by the
/// postprocess pipeline to reject the key when the current `base_url`
/// classifies to a different provider -- Codex P1 #642.
pub const POST_API_KEY_ENDPOINT_ENV: &str = "VOICEPI_POST_API_KEY_ENDPOINT";

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
        api_key_endpoint: get(POST_API_KEY_ENDPOINT_ENV).unwrap_or_default(),
        lang: get(LANG_ENV).unwrap_or_default(),
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
#[path = "settings_tests.rs"]
mod settings_tests;
