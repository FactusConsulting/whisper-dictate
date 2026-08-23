//! OpenAI-compatible `/chat/completions` client used by the post-processor
//! (and exposed to Python as the hidden `external-api` subcommand).
//!
//! Port of `openai_chat_completion()` from `vp_external_api.py` (Wave 4-B of
//! the Python-removal roadmap #348). Mirrors the Python behaviour exactly
//! (system prompt, message shape, temperature 0, return value) so the shell-
//! out can be swapped in transparently when
//! `VOICEPI_EXTERNAL_API_BACKEND=rust` is set.

use std::io::{self, Read};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cloud_api::http::{check_status, platform_tls_agent, CloudCallError, USER_AGENT};
use crate::cloud_api::prompts::POSTPROCESS_SYSTEM_PROMPT;
use crate::cloud_api::transcribe::{cap_transcription_prompt, GROQ_TRANSCRIPTION_PROMPT_LIMIT};

pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatCompletionResult {
    pub text: String,
    pub latency_ms: u64,
}

/// JSON envelope for the hidden `external-api` subcommand.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ExternalApiRequest {
    /// Post-processing cleanup (OpenAI-compatible `/chat/completions`).
    ChatCompletion {
        base_url: String,
        api_key: String,
        model: String,
        prompt: String,
        timeout_ms: u64,
    },
    /// Provider-specific cap for the transcription `prompt` field. The Python
    /// caller (`vp_external_api._cap_transcription_prompt`) already runs this
    /// pure-string helper inline, but exposing it via the envelope keeps the
    /// surface symmetric and lets future callers reuse the rule without
    /// reimplementing the byte-length math.
    CapTranscriptionPrompt { prompt: String, base_url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CapTranscriptionPromptResponse {
    prompt: String,
    limit: Option<usize>,
}

/// Classified result envelope for the `chat_completion` action.
///
/// The chat call no longer aborts the process on failure; instead it emits
/// this envelope on stdout (exit 0) so the Python shell-out
/// (`vp_external_api._rust_openai_chat_completion`) can decide what to do —
/// exactly like the `postprocess` verb. `kind` splits failures the same way
/// [`CloudCallError`] does: `"transport"` (provider never reached → Python may
/// safely retry via `urllib`) vs `"terminal"` (provider reached / bad body /
/// ambiguous timeout → Python must NOT retry, to avoid a duplicate charge).
/// On success `kind`/`error` are empty. The legacy `text`/`latency_ms` fields
/// are preserved so an older caller still reads them on the success path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ChatCompletionResponse {
    ok: bool,
    text: String,
    latency_ms: u64,
    kind: String,
    error: String,
}

impl ChatCompletionResponse {
    fn success(result: ChatCompletionResult) -> Self {
        Self {
            ok: true,
            text: result.text,
            latency_ms: result.latency_ms,
            kind: String::new(),
            error: String::new(),
        }
    }

    fn failure(err: &CloudCallError) -> Self {
        Self {
            ok: false,
            text: String::new(),
            latency_ms: 0,
            kind: if err.is_transport() {
                "transport"
            } else {
                "terminal"
            }
            .to_owned(),
            error: err.message().to_owned(),
        }
    }
}

pub fn handle_external_api() -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let request: ExternalApiRequest = serde_json::from_str(&raw)?;
    match request {
        ExternalApiRequest::ChatCompletion {
            base_url,
            api_key,
            model,
            prompt,
            timeout_ms,
        } => {
            // Do NOT abort on a chat failure — classify it and emit the
            // envelope so the Python caller can fall through (transport) or
            // surface the error (terminal) without a duplicate provider call.
            let response =
                match openai_chat_completion(&base_url, &api_key, &model, &prompt, timeout_ms) {
                    Ok(result) => ChatCompletionResponse::success(result),
                    Err(err) => ChatCompletionResponse::failure(&err),
                };
            println!("{}", serde_json::to_string(&response)?);
        }
        ExternalApiRequest::CapTranscriptionPrompt { prompt, base_url } => {
            let capped = cap_transcription_prompt(&prompt, &base_url).to_owned();
            let limit = if base_url.to_ascii_lowercase().contains("api.groq.com") {
                Some(GROQ_TRANSCRIPTION_PROMPT_LIMIT)
            } else {
                None
            };
            let response = CapTranscriptionPromptResponse {
                prompt: capped,
                limit,
            };
            println!("{}", serde_json::to_string(&response)?);
        }
    }
    Ok(())
}

/// OpenAI-compatible chat completion (the post-processor's cloud backend).
///
/// Mirrors `openai_chat_completion()` in `vp_external_api.py`:
/// * trims the base URL,
/// * sends the same system + user message pair with `temperature = 0`,
/// * returns the trimmed `choices[0].message.content` plus the wall-clock
///   latency in milliseconds.
pub fn openai_chat_completion(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    timeout_ms: u64,
) -> Result<ChatCompletionResult, CloudCallError> {
    if api_key.trim().is_empty() {
        return Err(CloudCallError::Terminal(
            "openai chat completion requires a non-empty API key (set VOICEPI_POST_API_KEY, \
             VOICEPI_STT_API_KEY, OPENAI_API_KEY, or GROQ_API_KEY)"
                .to_owned(),
        ));
    }
    if model.trim().is_empty() {
        return Err(CloudCallError::Terminal(
            "openai chat completion requires a non-empty model".to_owned(),
        ));
    }
    let base_url = base_url.trim_end_matches('/');
    let url = format!("{base_url}/chat/completions");
    let payload = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": POSTPROCESS_SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
        "temperature": 0,
    });
    let started = Instant::now();
    let mut response = platform_tls_agent()
        .post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(Duration::from_millis(timeout_ms.max(1000))))
        .http_status_as_error(false)
        .build()
        .send_json(payload)
        .map_err(|err| {
            CloudCallError::from_send(
                &format!("{} chat completion failed", provider_name(&url)),
                err,
            )
        })?;
    check_status(&mut response).map_err(|detail| {
        CloudCallError::Terminal(format!(
            "{} chat completion failed: {detail}",
            provider_name(&url)
        ))
    })?;
    let body: Value = response.body_mut().read_json().map_err(|err| {
        CloudCallError::Terminal(format!(
            "{} chat completion returned invalid JSON: {err}",
            provider_name(&url)
        ))
    })?;
    let text = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    Ok(ChatCompletionResult {
        text,
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

fn provider_name(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.contains("groq.com") {
        "Groq"
    } else if lower.contains("openai.com") {
        "OpenAI"
    } else {
        "external API"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn empty_api_key_is_rejected_before_network() {
        let err = openai_chat_completion("https://api.openai.com/v1", " ", "gpt", "x", 1000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-empty API key"));
    }

    #[test]
    fn empty_model_is_rejected_before_network() {
        let err = openai_chat_completion("https://api.openai.com/v1", "key", " ", "x", 1000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-empty model"));
    }

    #[test]
    fn empty_key_and_model_classify_as_terminal() {
        // A pre-network rejection is deterministic and terminal. (The
        // transport-vs-terminal classification of live send errors is covered
        // by robust, network-free unit tests in `cloud_api::http`, since a
        // refused-port connect behaves differently across platforms — Windows
        // times out where Linux refuses immediately.)
        let err =
            openai_chat_completion("https://api.openai.com/v1", " ", "gpt", "x", 1000).unwrap_err();
        assert!(!err.is_transport());
    }

    #[test]
    fn chat_response_maps_success_and_failure_kinds() {
        let ok = ChatCompletionResponse::success(ChatCompletionResult {
            text: "hi".to_owned(),
            latency_ms: 5,
        });
        assert!(ok.ok);
        assert_eq!(ok.text, "hi");
        assert!(ok.kind.is_empty());

        let transport = ChatCompletionResponse::failure(&CloudCallError::Transport("x".to_owned()));
        assert!(!transport.ok);
        assert_eq!(transport.kind, "transport");
        assert_eq!(transport.error, "x");

        let terminal = ChatCompletionResponse::failure(&CloudCallError::Terminal("y".to_owned()));
        assert!(!terminal.ok);
        assert_eq!(terminal.kind, "terminal");
    }

    #[test]
    fn provider_name_recognises_known_hosts() {
        assert_eq!(provider_name("https://api.groq.com/openai/v1/x"), "Groq");
        assert_eq!(provider_name("https://api.openai.com/v1/x"), "OpenAI");
        assert_eq!(provider_name("http://localhost/v1/x"), "external API");
    }

    /// End-to-end test against a tiny stub HTTP server bound to localhost.
    /// Exercises the request shape (path, auth, system+user messages,
    /// temperature) and the response unwrapping. Mirrors the Python
    /// `test_openai_postprocessor_uses_fake_chat_server` assertions.
    #[test]
    fn chat_completion_against_stub_server_returns_trimmed_content() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<String>();

        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream);
                // Read headers line-by-line until the blank line, then read
                // exactly Content-Length bytes from the body. TCP may split
                // ureq's request across reads, so a single `read()` would be
                // racy.
                let mut headers = String::new();
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some(rest) = line
                        .strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                    {
                        content_length = rest.trim().parse().unwrap_or(0);
                    }
                    headers.push_str(&line);
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let mut body_buf = vec![0u8; content_length];
                if content_length > 0 {
                    reader.read_exact(&mut body_buf).ok();
                }
                let mut request = headers;
                request.push_str(&String::from_utf8_lossy(&body_buf));
                let _ = tx.send(request);

                let body = r#"{"choices":[{"message":{"content":"  Cleaned text.  "}}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = reader.get_mut().write_all(response.as_bytes());
            }
        });

        let result = openai_chat_completion(
            &format!("http://127.0.0.1:{port}/v1"),
            "test-key",
            "gpt-4o-mini",
            "clean this",
            5_000,
        )
        .expect("chat completion succeeds");

        assert_eq!(result.text, "Cleaned text.");
        let request = rx.recv().expect("server received request");
        assert!(
            request.starts_with("POST /v1/chat/completions"),
            "unexpected request line: {request}"
        );
        // Match case-insensitively because ureq normalises header casing in
        // ways that differ between releases. The Python test fixture is the
        // source of truth for behaviour, but a header-casing mismatch should
        // not break the Rust unit test.
        let lower = request.to_ascii_lowercase();
        assert!(
            lower.contains("authorization: bearer test-key"),
            "missing Authorization header in: {request}"
        );
        assert!(request.contains("You rewrite dictated text faithfully."));
        assert!(
            lower.contains("\"temperature\":0") || lower.contains("\"temperature\": 0"),
            "missing temperature=0 in body: {request}"
        );
        assert!(request.contains("clean this"));
    }
}
