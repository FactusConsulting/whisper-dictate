//! Tests for [`super`] (`dictate::backends::cloud`).
//!
//! Three layers of coverage:
//!
//! 1. **Env → config resolution**: precedence + fallback rules for the
//!    `VOICEPI_STT_*` env vars, exercised against a serialised
//!    `ENV_LOCK` guard so parallel tests do not clobber each other.
//! 2. **`new()` validation**: empty base URL / API key / model each
//!    surface a distinct, actionable error string.
//! 3. **PCM → WAV encoding + full HTTP round-trip**: a tiny stub
//!    server (same pattern as `cloud_api::chat` tests) captures the
//!    multipart body and returns a canned JSON response so the
//!    `TranscribeBackend::transcribe` end-to-end contract is pinned.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

use hound::WavReader;

use super::{
    default_stt_model, encode_pcm_as_wav, CloudBackendConfig, CloudTranscribeBackend,
    DEFAULT_GROQ_STT_MODEL, DEFAULT_OPENAI_STT_MODEL, GROQ_API_KEY_ENV, INITIAL_PROMPT_ENV,
    LANG_ENV, OPENAI_API_KEY_ENV, STT_API_KEY_ENV, STT_BASE_URL_ENV, STT_MODEL_ENV,
    STT_TIMEOUT_MS_ENV,
};
use crate::dictate::session::types::{TranscribeBackend, TranscribeError};
use crate::test_env_lock::ENV_LOCK;

/// Restore an env var when the guard drops. Local copy of the same
/// pattern used across `runtime/*_tests.rs` -- we don't share the
/// helper because the sibling files live behind different feature
/// gates and dragging in `runtime` module types here would couple this
/// unit-test file to features it does not need.
struct EnvVarGuard {
    name: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let prev = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, prev }
    }

    fn unset(name: &'static str) -> Self {
        let prev = std::env::var(name).ok();
        std::env::remove_var(name);
        Self { name, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.name, v),
            None => std::env::remove_var(self.name),
        }
    }
}

// ── env → config ─────────────────────────────────────────────────────────────

#[test]
fn config_from_env_defaults_to_openai_when_base_url_unset() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::unset(STT_BASE_URL_ENV);
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "gpt-4o-mini-transcribe");
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "sk-test");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.base_url, "https://api.openai.com/v1");
    assert_eq!(cfg.model, "gpt-4o-mini-transcribe");
    assert_eq!(cfg.api_key, "sk-test");
    assert_eq!(cfg.timeout_ms, 30_000);
    assert!(cfg.language.is_none());
    assert!(cfg.initial_prompt.is_none());
}

#[test]
fn config_from_env_prefers_groq_key_when_base_url_is_groq() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.groq.com/openai/v1");
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "whisper-large-v3-turbo");
    let _stt_key = EnvVarGuard::unset(STT_API_KEY_ENV);
    let _groq_key = EnvVarGuard::set(GROQ_API_KEY_ENV, "gsk-secret");
    let _openai_key = EnvVarGuard::set(OPENAI_API_KEY_ENV, "sk-should-not-win");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.api_key, "gsk-secret");
}

#[test]
fn config_from_env_falls_back_to_openai_key_for_openai_base_url() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.openai.com/v1");
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "gpt-4o-mini-transcribe");
    let _stt_key = EnvVarGuard::unset(STT_API_KEY_ENV);
    let _groq_key = EnvVarGuard::set(GROQ_API_KEY_ENV, "gsk-should-not-win");
    let _openai_key = EnvVarGuard::set(OPENAI_API_KEY_ENV, "sk-fallback");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.api_key, "sk-fallback");
}

#[test]
fn config_from_env_stt_api_key_wins_regardless_of_provider() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.groq.com/openai/v1");
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "whisper-large-v3-turbo");
    let _stt_key = EnvVarGuard::set(STT_API_KEY_ENV, "primary");
    let _groq_key = EnvVarGuard::set(GROQ_API_KEY_ENV, "not-me");
    let _openai_key = EnvVarGuard::set(OPENAI_API_KEY_ENV, "not-me");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.api_key, "primary");
}

#[test]
fn config_from_env_timeout_is_floored_at_1000ms() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::unset(STT_BASE_URL_ENV);
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "m");
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "k");
    let _timeout = EnvVarGuard::set(STT_TIMEOUT_MS_ENV, "1");
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.timeout_ms, 1_000);
}

#[test]
fn config_from_env_language_and_prompt_collapse_blank_to_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::unset(STT_BASE_URL_ENV);
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "m");
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "k");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::set(LANG_ENV, "   ");
    let _prompt = EnvVarGuard::set(INITIAL_PROMPT_ENV, "");

    let cfg = CloudBackendConfig::from_env();
    assert!(cfg.language.is_none());
    assert!(cfg.initial_prompt.is_none());
}

// ── Codex #441 P2 round 3: default STT model for cloud providers ────────────
//
// Python's `vp_external_api.load_stt_api_settings` supplied a
// provider-specific default when `stt_model` was empty. The Rust cloud
// backend requires a non-empty model at `new()` time, so without the
// default a minimal `{stt_backend: "openai"}` config ended up in the
// stub sink with every press producing `no_text`.

#[test]
fn default_stt_model_maps_openai_base_url_to_openai_default() {
    // The stock OpenAI base URL falls through to the OpenAI default.
    assert_eq!(
        default_stt_model("https://api.openai.com/v1"),
        DEFAULT_OPENAI_STT_MODEL
    );
    assert_eq!(
        default_stt_model(DEFAULT_OPENAI_STT_MODEL),
        DEFAULT_OPENAI_STT_MODEL
    ); // any non-groq
}

#[test]
fn default_stt_model_maps_groq_base_url_to_groq_default() {
    // Any base URL containing `api.groq.com` -> Groq default. Case-
    // insensitive so `API.GROQ.COM` still routes correctly.
    assert_eq!(
        default_stt_model("https://api.groq.com/openai/v1"),
        DEFAULT_GROQ_STT_MODEL
    );
    assert_eq!(
        default_stt_model("https://API.GROQ.COM/openai/v1"),
        DEFAULT_GROQ_STT_MODEL
    );
}

#[test]
fn config_from_env_defaults_model_to_openai_when_backend_is_openai() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::unset(STT_BASE_URL_ENV);
    let _model = EnvVarGuard::unset(STT_MODEL_ENV);
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "sk-test");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.model, DEFAULT_OPENAI_STT_MODEL);
    // Backend also builds without the caller having to remember the
    // default — the whole point of the round-3 fix.
    assert!(CloudTranscribeBackend::new(cfg).is_ok());
}

#[test]
fn config_from_env_defaults_model_to_groq_when_base_url_is_groq() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.groq.com/openai/v1");
    let _model = EnvVarGuard::unset(STT_MODEL_ENV);
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "gsk-test");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.model, DEFAULT_GROQ_STT_MODEL);
    assert!(CloudTranscribeBackend::new(cfg).is_ok());
}

#[test]
fn config_from_env_explicit_model_wins_over_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::set(STT_BASE_URL_ENV, "https://api.groq.com/openai/v1");
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "distil-whisper");
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "gsk-test");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.model, "distil-whisper");
}

#[test]
fn config_from_env_blank_model_still_uses_provider_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _base = EnvVarGuard::unset(STT_BASE_URL_ENV);
    // Whitespace-only model must collapse to the default, not
    // propagate as a literal empty-after-trim value into the config.
    let _model = EnvVarGuard::set(STT_MODEL_ENV, "   ");
    let _key = EnvVarGuard::set(STT_API_KEY_ENV, "sk-test");
    let _timeout = EnvVarGuard::unset(STT_TIMEOUT_MS_ENV);
    let _lang = EnvVarGuard::unset(LANG_ENV);
    let _prompt = EnvVarGuard::unset(INITIAL_PROMPT_ENV);

    let cfg = CloudBackendConfig::from_env();
    assert_eq!(cfg.model, DEFAULT_OPENAI_STT_MODEL);
}

// ── new() validation ─────────────────────────────────────────────────────────

#[test]
fn new_rejects_empty_base_url_before_network() {
    let err = CloudTranscribeBackend::new(CloudBackendConfig {
        base_url: String::new(),
        api_key: "k".to_owned(),
        model: "m".to_owned(),
        language: None,
        initial_prompt: None,
        timeout_ms: 30_000,
    })
    .unwrap_err();
    assert!(err.contains("base URL"), "unexpected error: {err}");
}

#[test]
fn new_rejects_empty_api_key_before_network() {
    let err = CloudTranscribeBackend::new(CloudBackendConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: "   ".to_owned(),
        model: "m".to_owned(),
        language: None,
        initial_prompt: None,
        timeout_ms: 30_000,
    })
    .unwrap_err();
    assert!(err.contains("API key"), "unexpected error: {err}");
}

#[test]
fn new_rejects_empty_model_before_network() {
    let err = CloudTranscribeBackend::new(CloudBackendConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: "k".to_owned(),
        model: String::new(),
        language: None,
        initial_prompt: None,
        timeout_ms: 30_000,
    })
    .unwrap_err();
    assert!(err.contains("model"), "unexpected error: {err}");
}

// ── PCM → WAV encoding ───────────────────────────────────────────────────────

#[test]
fn encode_pcm_as_wav_produces_16k_mono_16bit() {
    let pcm: Vec<f32> = (0..16_000).map(|i| ((i as f32) / 16_000.0) * 0.5).collect();
    let bytes = encode_pcm_as_wav(&pcm, 16_000).expect("encode succeeds");

    let cursor = std::io::Cursor::new(bytes);
    let reader = WavReader::new(cursor).expect("output is a valid WAV");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
}

#[test]
fn encode_pcm_as_wav_clamps_out_of_range_samples() {
    // A caller that fed us mastered audio at 1.5 would otherwise wrap
    // the i16 conversion; verify the clamp keeps the peak at i16::MAX.
    let pcm = vec![1.5_f32, -1.5, 0.0];
    let bytes = encode_pcm_as_wav(&pcm, 16_000).expect("encode succeeds");
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = WavReader::new(cursor).expect("valid WAV");
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .expect("readable samples");
    assert_eq!(samples, vec![i16::MAX, -i16::MAX, 0]);
}

#[test]
fn transcribe_rejects_zero_sample_rate() {
    let backend = CloudTranscribeBackend::new(CloudBackendConfig {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: "k".to_owned(),
        model: "m".to_owned(),
        language: None,
        initial_prompt: None,
        timeout_ms: 30_000,
    })
    .expect("backend builds");
    let err = backend.transcribe(&[0.0], 0).unwrap_err();
    match err {
        TranscribeError::Backend(msg) => {
            assert!(msg.contains("sample_rate"), "unexpected message: {msg}")
        }
    }
}

// ── end-to-end HTTP round-trip ───────────────────────────────────────────────

/// Bind a localhost TCP socket, accept exactly one connection, capture
/// the raw request bytes (headers + Content-Length body), and reply
/// with the supplied JSON body. Returns the port + a channel receiver
/// that yields the captured request once the client disconnects.
///
/// Modelled on `cloud_api::chat`'s `chat_completion_against_stub_server`
/// helper; kept inline so this test file compiles standalone.
fn spawn_stub_server(body: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel::<String>();

    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
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
            // Preserve raw bytes for the multipart-body assertion --
            // header lines are always ASCII; the body may contain
            // arbitrary bytes but we only assert on ASCII substrings
            // so lossy conversion is fine here.
            let mut request = headers;
            request.push_str(&String::from_utf8_lossy(&body_buf));
            let _ = tx.send(request);

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = reader.get_mut().write_all(response.as_bytes());
        }
    });

    (port, rx)
}

#[test]
fn transcribe_end_to_end_against_stub_server() {
    let body = r#"{"text":"  hello, cloud world  ","language":"en"}"#;
    let (port, rx) = spawn_stub_server(body);
    let backend = CloudTranscribeBackend::new(CloudBackendConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        api_key: "test-key".to_owned(),
        model: "gpt-4o-mini-transcribe".to_owned(),
        language: Some("da".to_owned()),
        initial_prompt: Some("Voicepi Factus".to_owned()),
        timeout_ms: 5_000,
    })
    .expect("backend builds");

    // 100 ms of silence at 16 kHz: enough to prove the request carries a
    // WAV body, without shipping a real audio fixture.
    let pcm = vec![0.0_f32; 1_600];
    let result = backend
        .transcribe(&pcm, 16_000)
        .expect("stub server responds successfully");
    assert_eq!(result.text, "hello, cloud world");
    assert_eq!(result.language, "en");
    // `duration_s = 1600 / 16000 = 0.1`.
    assert!(
        (result.duration_s - 0.1).abs() < 1e-9,
        "unexpected duration_s: {}",
        result.duration_s
    );

    let request = rx.recv().expect("server received a request");
    assert!(
        request.starts_with("POST /v1/audio/transcriptions"),
        "unexpected request line: {request}"
    );
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.contains("authorization: bearer test-key"),
        "missing Authorization header: {request}"
    );
    assert!(
        lower.contains("content-type: multipart/form-data;"),
        "expected multipart Content-Type: {request}"
    );
    assert!(
        request.contains("name=\"model\""),
        "model field missing from body: {request}"
    );
    assert!(
        request.contains("gpt-4o-mini-transcribe"),
        "model value missing from body: {request}"
    );
    assert!(
        request.contains("name=\"language\""),
        "language field missing from body: {request}"
    );
    assert!(
        request.contains("da"),
        "language value missing from body: {request}"
    );
    assert!(
        request.contains("name=\"prompt\""),
        "prompt field missing from body: {request}"
    );
    assert!(
        request.contains("Voicepi Factus"),
        "prompt value missing from body: {request}"
    );
    assert!(
        request.contains("filename=\"audio.wav\""),
        "file part missing from body"
    );
}

#[test]
fn transcribe_surfaces_network_error_as_transcribe_error() {
    // Bind + immediately drop so the port is reserved by the OS for the
    // duration of the connect attempt but no one accepts -- most
    // platforms will surface this as `connection refused`. Some Linux
    // kernels queue the SYN silently; the assertion only pins that the
    // error round-trips as a `TranscribeError::Backend`, not the exact
    // wording of the transport-level failure.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let backend = CloudTranscribeBackend::new(CloudBackendConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        api_key: "k".to_owned(),
        model: "m".to_owned(),
        language: None,
        initial_prompt: None,
        // Keep the timeout short so the test finishes quickly even if
        // the platform queues the SYN.
        timeout_ms: 1_500,
    })
    .expect("backend builds");

    let pcm = vec![0.0_f32; 1_600];
    let err = backend
        .transcribe(&pcm, 16_000)
        .expect_err("dead port must not succeed");
    match err {
        TranscribeError::Backend(msg) => {
            assert!(
                !msg.is_empty(),
                "network failure should surface a non-empty message"
            );
        }
    }
}
