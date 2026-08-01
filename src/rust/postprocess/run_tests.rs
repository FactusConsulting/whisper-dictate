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
        lang: String::new(),
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
    // for the boundary cases this parity check covers — the Rust answer
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

/// Serve exactly ONE canned Ollama `/api/generate` response from a loopback
/// socket and hand the received REQUEST BODY back over a channel, so a test
/// can assert on the prompt the pipeline actually sent (the model's answer is
/// not deterministic; the prompt contract is). Returns the ephemeral port and
/// the receiving end of the body channel.
fn serve_one_ollama_response(reply: &str) -> (u16, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel::<String>();
    let payload = serde_json::json!({ "response": reply }).to_string();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(clone);
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let _ = tx.send(String::from_utf8_lossy(&body).into_owned());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    (port, rx)
}

#[test]
fn provider_request_carries_the_configured_language_in_the_prompt() {
    // #685 wiring regression: `build_prompt` growing a language paragraph is
    // worthless if the pipeline never passes `settings.lang` to it. Serve a
    // canned Ollama response from a loopback socket and assert the REQUEST
    // BODY the pipeline actually sent names the configured language and pins
    // the numerals. (The model's answer cannot be asserted deterministically;
    // the prompt contract can.)
    use std::time::Duration;

    let (port, rx) = serve_one_ollama_response("1, 2, 3, 4, 5, 6");

    let mut settings = sample("ollama", "clean", &format!("http://127.0.0.1:{port}"));
    settings.lang = "da".to_owned();
    settings.timeout_ms = 5_000;
    let result = postprocess_text("1, 2, 3, 4, 5, 6", &settings);

    let body = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("provider never received a request");
    assert!(
        body.contains("the input is in da (ISO 639-1 code)"),
        "prompt did not name the configured language: {body}"
    );
    assert!(
        body.contains("Never translate the text or switch to another language"),
        "prompt did not forbid translation: {body}"
    );
    assert!(
        body.contains("do not convert digits into words or words into digits"),
        "prompt did not pin numerals: {body}"
    );
    assert!(!result.fallback, "unexpected fallback: {}", result.error);
    assert_eq!(result.text, "1, 2, 3, 4, 5, 6");
}

#[test]
fn provider_request_carries_the_per_utterance_language_not_the_configured_one() {
    // Pin the same seam used by the in-process engine: a session whose
    // settings were built from
    // `VOICEPI_LANG=da` runs ONE utterance that STT transcribed as English
    // (a `--lang en` run, an English per-application profile, or an
    // auto-detect hit). The request body must name `en`, not the session's
    // configured `da`, so post-processing follows the actual utterance.
    use crate::dictate::PostProcessBackend;
    use crate::postprocess::SessionPostProcess;
    use std::time::Duration;

    let (port, rx) = serve_one_ollama_response("Hello there.");

    let mut configured = sample("ollama", "clean", &format!("http://127.0.0.1:{port}"));
    configured.lang = "da".to_owned();
    configured.timeout_ms = 5_000;
    let backend = SessionPostProcess::from_settings(configured);

    let outcome = backend.post_process("hello there", "en");

    let body = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("provider never received a request");
    assert!(
        body.contains("the input is in en (ISO 639-1 code)"),
        "prompt must name the language STT actually used: {body}"
    );
    assert!(
        !body.contains("the input is in da"),
        "prompt must not name the stale configured language: {body}"
    );
    assert!(!outcome.fallback, "unexpected fallback: {}", outcome.error);
    assert_eq!(outcome.text, "Hello there.");
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
