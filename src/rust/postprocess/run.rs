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

/// Codex P1 #642 (+ #666 P1 sweep #3 / #4): refuse to send an injected key
/// to an endpoint that does not match the marker the launcher stamped for
/// it. The check is deliberately strict on three axes because relaxing any
/// of them re-opens a distinct leak channel:
///
/// * **Provider**: Groq marker + OpenAI base_url (or Custom) => reject. The
///   Codex P1 #642 headline.
/// * **Scheme (Codex P1 #666 #3, `PRRT_kwDOSfNjQs6UXpn3`)**: an https
///   marker + http base_url => reject. Both HTTP implementations attach
///   the Bearer to the initial unencrypted request, so an attacker who
///   can rewrite the URL to http:// can observe / intercept the key
///   regardless of a later redirect. Downgrade => refuse, period.
/// * **Custom origin (Codex P1 #666 #4, `PRRT_kwDOSfNjQs6UXpnz`)**: two
///   different self-hosted hosts both classify as `Custom`. When the marker
///   is Custom, compare EXACT origin (scheme + host + port) so a live change
///   from `https://a.example` to `https://b.example` is rejected. A prior
///   version treated Custom==Custom as always-allow because
///   `attach_cloud_api_keys` was assumed never to stamp a Custom marker;
///   that assumption was wrong (the STT-as-post fallback in
///   `credentials::resolve_post_api_key` can inject a shared key for a
///   Custom post endpoint, and `App::worker_command` in the UI likewise
///   pushes a key against whatever `post_base_url` the user configured).
///
/// * `Ok(())` when there is no marker (backward compat: a user who exported
///   their own `VOICEPI_POST_API_KEY` owns the resolution).
/// * `Err(message)` on any of the three mismatches above.
///
/// Pure function -- takes only strings, returns only strings. All the HTTP /
/// provider dispatch stays in the caller so the check is exhaustively
/// unit-tested without any network.
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
    // Same provider (or both Custom). Now enforce the two additional axes
    // relaxing either of which re-opens a distinct leak (see doc comment).
    let base_parts = origin_parts(base_url);
    let marker_parts = origin_parts(marker);
    // Scheme downgrade: an https marker must not send to a http base_url.
    // (An http marker -> https base_url is a legitimate upgrade -- allow.)
    if marker_parts.scheme.eq_ignore_ascii_case("https")
        && base_parts.scheme.eq_ignore_ascii_case("http")
    {
        return Err(format!(
            "refusing to send stored post-processing key over plaintext http:// \
             (Codex P1 #666 #3): marker requires https ({marker:?}) but current base URL \
             downgrades to http ({base_url:?}). An attacker able to observe the initial \
             request would capture the Bearer token even if the server later redirects to \
             https. Restore the https endpoint or restart the worker."
        ));
    }
    if marker_provider == Provider::Custom {
        // Custom marker: require exact scheme+host+port match. Two custom
        // hosts share the Custom classification, so a live change from one
        // custom origin to another would otherwise permit the key travel.
        if !base_parts.same_origin(&marker_parts) {
            return Err(format!(
                "refusing to send stored post-processing key to a different self-hosted \
                 origin (Codex P1 #666 #4): key was resolved for {marker:?} but current \
                 base URL is {base_url:?}. Self-hosted endpoints have no cross-account \
                 trust; update the API key for the new host or restart the worker."
            ));
        }
    }
    Ok(())
}

/// Parsed origin for [`require_endpoint_matches_marker`] -- kept as a plain
/// struct so the check can compare hosts/ports without pulling in a URL
/// crate. Mirrors the "pragmatic URL parsing" the rest of this module already
/// does (`looks_like_http_url`, `Provider::from_base_url`).
#[derive(Debug, Clone, Default)]
struct OriginParts {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl OriginParts {
    fn same_origin(&self, other: &Self) -> bool {
        self.scheme.eq_ignore_ascii_case(&other.scheme)
            && self.host.eq_ignore_ascii_case(&other.host)
            && self.effective_port() == other.effective_port()
    }
    fn effective_port(&self) -> u16 {
        self.port.unwrap_or_else(|| {
            if self.scheme.eq_ignore_ascii_case("https") {
                443
            } else {
                80
            }
        })
    }
}

fn origin_parts(url: &str) -> OriginParts {
    // Reuse the SAME classifier as `Provider::from_base_url` (host by
    // `provider_host_public`) so scheme + host land the same everywhere.
    // Empty host for a malformed URL falls through to a mismatch: fail-closed.
    let scheme = url.split("://").next().unwrap_or("").to_ascii_lowercase();
    let host = crate::cloud_api::provider_host_public(url)
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Port extracted from the authority section. Handles the same IPv6 /
    // userinfo shapes the classifier does: `scheme://user@[v6]:port/` and
    // `scheme://user@host:port/`.
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port = match host_port.strip_prefix('[') {
        Some(rest) => rest
            .split_once(']')
            .and_then(|(_, tail)| tail.strip_prefix(':'))
            .and_then(|p| p.parse::<u16>().ok()),
        None => host_port
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok()),
    };
    OriginParts { scheme, host, port }
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
