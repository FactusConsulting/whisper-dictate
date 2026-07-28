//! The actual post-processing pipeline: validate → redact → call provider →
//! restore redactions → cap output. The two HTTP backends (Ollama
//! `/api/generate` and OpenAI-compatible `/chat/completions`) live here too;
//! the chat completion is shared with the hidden `external-api` subcommand
//! via [`crate::cloud_api::openai_chat_completion`].

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::cloud_api::http::{platform_tls_agent, CloudCallError};
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
    /// Why the call fell back, when `fallback` is true: `"transport"` (request
    /// never reached the provider — the Python path may retry safely) or
    /// `"terminal"` (provider reached / ambiguous timeout / config rejection —
    /// do not retry). Empty when `fallback` is false. Consumed by the Python
    /// shell-out (`vp_postprocess._rust_postprocess_text`) to decide whether to
    /// fall through to `urllib`.
    pub fallback_kind: String,
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
            // A validation failure (bad processor/mode/URL, or a local-only
            // block) is deterministic — the Python path would reject it the
            // same way — so it is terminal, not a transport retry candidate.
            return fallback_result(
                text,
                settings,
                mode_short,
                0,
                "terminal",
                err,
                false,
                Vec::new(),
            );
        }
    };

    // Codex P1 #642: before any cloud call, refuse to send an
    // endpoint-mismatched injected key. The launcher stamps
    // `api_key_endpoint` with the URL the key was resolved for; if a live
    // `post_processor` / `post_base_url` change moved the current
    // `base_url` to a different provider, this stale key would otherwise
    // travel as a Bearer token to an unrelated host. Local processors
    // (`none` / `ollama`) never hit `openai_chat_completion`, so the check
    // is scoped to the cloud branch.
    if matches!(settings.processor.as_str(), "openai" | "groq") {
        if let Err(err) =
            require_endpoint_matches_marker(&settings.base_url, &settings.api_key_endpoint)
        {
            return fallback_result(text, settings, mode, 0, "terminal", err, false, Vec::new());
        }
    }

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
        other => Err(CloudCallError::Terminal(format!(
            "unsupported post processor: {other}"
        ))),
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
                fallback_kind: String::new(),
                error: String::new(),
                redacted: !redactions.is_empty(),
                redactions: redactions_summary(&redactions),
            }
        }
        Err(err) => {
            let kind = if err.is_transport() {
                "transport"
            } else {
                "terminal"
            };
            fallback_result(
                text,
                settings,
                mode,
                latency_ms,
                kind,
                err.message().to_owned(),
                !redactions.is_empty(),
                redactions_summary(&redactions),
            )
        }
    }
}

/// Codex P1 #642: refuse to send an injected key to an endpoint whose
/// provider does not match the marker the launcher stamped for it.
///
/// * `Ok(())` when there is no marker (backward compat: a user who exported
///   their own `VOICEPI_POST_API_KEY` owns the resolution), OR when the
///   current `base_url` classifies to the same provider as the marker (same
///   provider, different URL is fine -- e.g. Groq default vs. Groq beta URL).
/// * `Err(message)` when the marker is set and the providers differ,
///   including the Custom-vs-Groq case (a live change to a self-hosted host
///   must not receive the stored provider key).
///
/// Pure function: takes only strings, returns only strings. All the HTTP /
/// provider dispatch stays in the caller so this check can be exhaustively
/// unit-tested.
fn require_endpoint_matches_marker(base_url: &str, marker: &str) -> Result<(), String> {
    let marker = marker.trim();
    if marker.is_empty() {
        return Ok(());
    }
    use crate::credentials::Provider;
    let base_provider = Provider::from_base_url(base_url);
    let marker_provider = Provider::from_base_url(marker);
    if base_provider != marker_provider {
        return Err(format!(
            "refusing to send stored post-processing key to a different endpoint: \
             key was resolved for {marker:?} ({marker_provider:?}) but current base URL is \
             {base_url:?} ({base_provider:?}). Update the API key for the new provider in \
             Settings, or restart the worker so the launcher resolves the right key."
        ));
    }
    // Custom-vs-Custom: two different self-hosted hosts share the Custom
    // classification. `attach_cloud_api_keys` never stamps a marker for a
    // Custom endpoint (`resolve_post_api_key` returns None for a Custom host
    // and `post_credential_and_endpoint` skips the marker in that case), so
    // reaching here with `marker_provider == Custom` means either a stale
    // marker crafted by hand or a nested spawn -- treat as advisory only,
    // matching the "user owns the resolution" rule for a manually set key.
    Ok(())
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
        fallback_kind: String::new(),
        error: String::new(),
        redacted: false,
        redactions: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn fallback_result(
    text: &str,
    settings: &PostprocessSettings,
    mode: String,
    latency_ms: u64,
    fallback_kind: &str,
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
        fallback_kind: fallback_kind.to_owned(),
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

fn ollama_generate(
    settings: &PostprocessSettings,
    text: &str,
    mode: &str,
) -> Result<String, CloudCallError> {
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
    let mut response = platform_tls_agent()
        .post(&url)
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
        .map_err(|err| CloudCallError::from_send("ollama post-processing failed", err))?;

    let code = response.status().as_u16();
    if !(200..300).contains(&code) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(CloudCallError::Terminal(format!(
            "ollama post-processing failed: HTTP {code}: {}",
            body.trim()
        )));
    }
    let body: Value = response.body_mut().read_json().map_err(|err| {
        CloudCallError::Terminal(format!(
            "ollama post-processing returned invalid JSON: {err}"
        ))
    })?;
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
#[path = "run_tests.rs"]
mod run_tests;
