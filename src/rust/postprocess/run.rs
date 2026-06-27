//! The actual post-processing pipeline: validate → redact → call provider →
//! restore redactions → cap output. The two HTTP backends (Ollama
//! `/api/generate` and OpenAI-compatible `/chat/completions`) live here too;
//! the chat completion is shared with the hidden `external-api` subcommand
//! via [`crate::cloud_api::openai_chat_completion`].

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::cloud_api::openai_chat_completion;
use crate::postprocess::prompt::{build_prompt, extract_final_text, normalize_mode};
use crate::postprocess::settings::{validate, PostprocessSettings};
use crate::redaction;

/// Per-character timeout budget (ms) added to the configured base.
pub const PER_CHAR_MS: u64 = 20;
/// Hard ceiling for the effective timeout, regardless of length.
pub const CEILING_MS: u64 = 30_000;

/// Length-scaled HTTP timeout for a cleanup call.
///
/// Mirrors the Python `effective_timeout_ms`:
/// `max(base_ms, min(scaled, CEILING_MS))`. The base acts as a hard floor
/// — short inputs never drop below the configured base, AND a base raised
/// above `CEILING_MS` is preserved unchanged (the user explicitly asked
/// for that floor because their local post-processing model is slow).
/// The ceiling only caps the per-character SCALING so a giant dictation
/// does not silently push the timeout to absurd values. P3 #382: the
/// settings schema allows `timeout_ms` up to 600 000, so the previous
/// `clamp(base, ceiling)` form silently degraded users with raised floors
/// when they switched to the Rust backend.
pub fn effective_timeout_ms(base_ms: u64, text_chars: i64) -> u64 {
    let chars = u64::try_from(text_chars.max(0)).unwrap_or(0);
    let scaled = base_ms.saturating_add(chars.saturating_mul(PER_CHAR_MS));
    // The ceiling only caps the SCALED-by-length value; the configured
    // base is then the floor on top of that, so `base_ms = 60_000` yields
    // 60 000 ms regardless of input length, matching the Python contract.
    base_ms.max(scaled.min(CEILING_MS))
}

#[derive(Debug, Clone, Serialize)]
pub struct PostprocessResult {
    pub text: String,
    pub raw_text: String,
    pub changed: bool,
    pub provider: String,
    pub mode: String,
    pub model: String,
    pub latency_ms: u64,
    pub fallback: bool,
    pub error: String,
    pub redacted: bool,
    /// Public-safe redaction summary (placeholder/kind/chars) — matches the
    /// Python `RedactionResult.public_summary()` shape so the existing
    /// metrics consumer keeps working.
    pub redactions: Vec<RedactionSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RedactionSummary {
    pub placeholder: String,
    pub kind: String,
    pub chars: usize,
}

/// Full post-processing pipeline. Returns a `PostprocessResult` whether the
/// provider succeeded, returned the original text unchanged, or fell back
/// after a transport error — same contract as the Python version.
pub fn postprocess_text(text: &str, settings: &PostprocessSettings) -> PostprocessResult {
    let mode_short = normalize_mode(&settings.mode);
    if settings.processor == "none" || mode_short == "raw" || text.trim().is_empty() {
        return raw_passthrough(text, settings, mode_short);
    }

    let mode = match validate(settings) {
        Ok(mode) => mode,
        Err(err) => {
            return fallback_result(text, settings, mode_short, 0, err, false, Vec::new());
        }
    };

    let clipped: String = text.chars().take(settings.max_input_chars).collect();
    let (prompt_text, redactions) = redact_for_cloud(&clipped, settings);
    let started = Instant::now();

    let outcome = match settings.processor.as_str() {
        "ollama" => ollama_generate(settings, &clipped, &mode),
        "openai" | "groq" => openai_chat_completion(
            &settings.base_url,
            &settings.api_key,
            &settings.model,
            &build_prompt(&prompt_text, &mode),
            effective_timeout_ms(settings.timeout_ms, prompt_text.chars().count() as i64),
        )
        .map(|res| res.text),
        other => Err(anyhow::anyhow!("unsupported post processor: {other}")),
    };

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match outcome {
        Ok(raw_output) => {
            let mut out = extract_final_text(&raw_output, &prompt_text);
            for r in &redactions {
                out = out.replace(&r.placeholder, &r.value);
            }
            let truncated: String = out.chars().take(settings.max_output_chars).collect();
            let trimmed = truncated.trim();
            let final_text = if trimmed.is_empty() {
                text.to_owned()
            } else {
                trimmed.to_owned()
            };
            PostprocessResult {
                text: final_text.clone(),
                raw_text: text.to_owned(),
                changed: final_text != text,
                provider: settings.processor.clone(),
                mode,
                model: settings.model.clone(),
                latency_ms,
                fallback: false,
                error: String::new(),
                redacted: !redactions.is_empty(),
                redactions: redactions_summary(&redactions),
            }
        }
        Err(err) => fallback_result(
            text,
            settings,
            mode,
            latency_ms,
            err.to_string(),
            !redactions.is_empty(),
            redactions_summary(&redactions),
        ),
    }
}

fn raw_passthrough(text: &str, settings: &PostprocessSettings, mode: String) -> PostprocessResult {
    PostprocessResult {
        text: text.to_owned(),
        raw_text: text.to_owned(),
        changed: false,
        provider: settings.processor.clone(),
        mode,
        model: settings.model.clone(),
        latency_ms: 0,
        fallback: false,
        error: String::new(),
        redacted: false,
        redactions: Vec::new(),
    }
}

fn fallback_result(
    text: &str,
    settings: &PostprocessSettings,
    mode: String,
    latency_ms: u64,
    error: String,
    redacted: bool,
    redactions: Vec<RedactionSummary>,
) -> PostprocessResult {
    PostprocessResult {
        text: text.to_owned(),
        raw_text: text.to_owned(),
        changed: false,
        provider: settings.processor.clone(),
        mode,
        model: settings.model.clone(),
        latency_ms,
        fallback: true,
        error,
        redacted,
        redactions,
    }
}

fn redact_for_cloud(
    text: &str,
    settings: &PostprocessSettings,
) -> (String, Vec<redaction::Redaction>) {
    if !matches!(settings.processor.as_str(), "openai" | "groq") || !settings.redact {
        return (text.to_owned(), Vec::new());
    }
    let terms: Vec<String> = settings
        .redact_terms
        .split(',')
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    let result = redaction::redact_text(text, &terms);
    (result.text, result.redactions)
}

fn redactions_summary(redactions: &[redaction::Redaction]) -> Vec<RedactionSummary> {
    redactions
        .iter()
        .map(|r| RedactionSummary {
            placeholder: r.placeholder.clone(),
            kind: r.kind.clone(),
            chars: r.value.chars().count(),
        })
        .collect()
}

fn ollama_generate(settings: &PostprocessSettings, text: &str, mode: &str) -> Result<String> {
    let url = format!("{}/api/generate", settings.base_url.trim_end_matches('/'));
    let num_predict = (settings.max_output_chars / 4).max(1);
    let payload = serde_json::json!({
        "model": settings.model,
        "prompt": build_prompt(text, mode),
        "stream": false,
        "options": {
            "temperature": 0,
            "num_predict": num_predict,
        },
    });
    let timeout = effective_timeout_ms(settings.timeout_ms, text.chars().count() as i64);
    let mut response = ureq::post(&url)
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            "whisper-dictate/0.3 (+https://github.com/FactusConsulting/whisper-dictate)",
        )
        .config()
        .timeout_global(Some(Duration::from_millis(timeout.max(1000))))
        .http_status_as_error(false)
        .build()
        .send_json(payload)
        .map_err(|err| anyhow::anyhow!("ollama post-processing failed: {err}"))?;

    let code = response.status().as_u16();
    if !(200..300).contains(&code) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(anyhow::anyhow!(
            "ollama post-processing failed: HTTP {code}: {}",
            body.trim()
        ));
    }
    let body: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| anyhow::anyhow!("ollama post-processing returned invalid JSON: {err}"))?;
    let response_text = body
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if response_text.is_empty() {
        Ok(text.to_owned())
    } else {
        Ok(response_text)
    }
}

#[cfg(test)]
mod tests {
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
    }

    #[test]
    fn invalid_processor_falls_back_with_validation_error() {
        let settings = sample("bogus", "clean", "http://127.0.0.1:1");
        let result = postprocess_text("hello", &settings);

        assert!(result.fallback);
        assert!(result.error.contains("invalid post processor"));
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
}
