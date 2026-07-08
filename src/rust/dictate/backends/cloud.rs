//! [`TranscribeBackend`] impl that POSTs audio to an OpenAI-compatible
//! `/audio/transcriptions` endpoint (OpenAI, Groq, or any self-hosted
//! server with the same wire format).
//!
//! Wave 5.5 gap #1 of #348. Before this module, setting
//! `stt_backend = "openai"` (or `stt_provider = "groq"`) with
//! `VOICEPI_DICTATE_BACKEND=rust-session` on a build with the full
//! feature set silently degraded: the real-backend factory built a
//! Whisper backend, tried to load a model the cloud user probably
//! didn't have on disk, and fell through to the stub sink so every
//! utterance came back as empty `no_text`. The Python worker was
//! never invoked because delegation ran anyway.
//!
//! This backend closes the gap by turning captured PCM into a
//! multipart/form-data upload against the configured base URL. All
//! config -- base URL, model, timeout, language hint, API key -- is
//! resolved from the same `VOICEPI_STT_*` env vars the Python cloud
//! path (`vp_external_api.py`) already reads, so a supervisor that
//! spawns the worker-rust subprocess with the resolved env sees
//! identical behaviour whether cloud transcription runs in Python or
//! in Rust.
//!
//! # No feature gate
//!
//! Unlike [`super::whisper_local`] (whisper-rs-local) and
//! [`super::inject`] (rust-injection), this module has no optional
//! dependency: `cloud_api::cloud_transcribe` is always compiled in.
//! Every build therefore knows how to speak the cloud protocol, and
//! the `whisper-rs-local` feature only gates the LOCAL-model path.

use std::io::Cursor;

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::cloud_api::cloud_transcribe;
use crate::dictate::session::types::{TranscribeBackend, TranscribeError, TranscribeResult};

/// Full per-call config the [`CloudTranscribeBackend`] carries. Loaded
/// once at session construction from the same `VOICEPI_STT_*` env vars
/// the Python worker reads (see [`CloudBackendConfig::from_env`]).
#[derive(Debug, Clone)]
pub struct CloudBackendConfig {
    /// Base URL, e.g. `https://api.openai.com/v1` or
    /// `https://api.groq.com/openai/v1`. Trailing `/` is stripped by
    /// the underlying HTTP client. Rejected as an empty string at
    /// [`CloudTranscribeBackend::new`] time so a misconfigured worker
    /// fails fast rather than crashing on the first press.
    pub base_url: String,
    /// Bearer token. Empty string is treated as a construction-time
    /// error -- the underlying `cloud_transcribe` rejects it too, but
    /// surfacing the failure at session build lets the supervisor emit
    /// a clear stderr line instead of every press producing a cryptic
    /// per-utterance error.
    pub api_key: String,
    /// Model identifier (e.g. `gpt-4o-mini-transcribe`,
    /// `whisper-large-v3-turbo`).
    pub model: String,
    /// Optional BCP-47-ish language hint. `None` / `Some("")` lets the
    /// server auto-detect.
    pub language: Option<String>,
    /// Optional initial-prompt biasing hint (dictionary-derived, etc.).
    pub initial_prompt: Option<String>,
    /// HTTP timeout in ms. Clamped to a 1 s floor by `cloud_transcribe`.
    pub timeout_ms: u64,
}

/// Env var carrying the base URL. Mirrors the Python worker's
/// env-var contract (`vp_external_api.load_stt_api_settings`).
pub const STT_BASE_URL_ENV: &str = "VOICEPI_STT_BASE_URL";
/// Env var carrying the model identifier.
pub const STT_MODEL_ENV: &str = "VOICEPI_STT_MODEL";
/// Env var carrying the primary API key. Consulted first regardless
/// of provider so a user with the shared key set once picks it up.
pub const STT_API_KEY_ENV: &str = "VOICEPI_STT_API_KEY";
/// Env var carrying the HTTP timeout in ms.
pub const STT_TIMEOUT_MS_ENV: &str = "VOICEPI_STT_TIMEOUT_MS";
/// Fallback env var for the OpenAI API key. Matches the Python
/// `_api_key` helper's precedence.
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
/// Fallback env var for the Groq API key. Only consulted when the
/// resolved base URL points at `api.groq.com`.
pub const GROQ_API_KEY_ENV: &str = "GROQ_API_KEY";
/// Language hint env var. Shared with the local Whisper backend.
pub const LANG_ENV: &str = "VOICEPI_LANG";
/// Initial-prompt env var. Shared with the local Whisper backend.
pub const INITIAL_PROMPT_ENV: &str = "VOICEPI_INITIAL_PROMPT";

/// Default base URL when the env var is unset. Kept in sync with
/// [`crate::cloud_api::DEFAULT_OPENAI_BASE_URL`] (which lives in
/// `cloud_api::chat`); re-declared here so `dictate::backends` does
/// not have to reach into the chat module for a single constant.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Default HTTP timeout in ms when the env var is unset / unparseable.
/// Matches the Python cloud path's default in `vp_external_api.py`.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Hard floor on the timeout so a misconfigured `1` ms does not turn
/// every press into a socket-level failure. Matches `cloud_transcribe`'s
/// own floor.
const TIMEOUT_FLOOR_MS: u64 = 1_000;

impl CloudBackendConfig {
    /// Resolve every field from the process env, mirroring the Python
    /// worker's `vp_external_api.load_stt_api_settings` precedence:
    ///
    /// 1. Base URL: `VOICEPI_STT_BASE_URL`, default openai.
    /// 2. Model: `VOICEPI_STT_MODEL`, no default (caller must supply
    ///    or `CloudTranscribeBackend::new` rejects).
    /// 3. API key: `VOICEPI_STT_API_KEY`, then `GROQ_API_KEY` if the
    ///    base URL points at `api.groq.com`, then `OPENAI_API_KEY`.
    /// 4. Timeout: `VOICEPI_STT_TIMEOUT_MS`, default 30 000, floored
    ///    at 1 000 ms.
    /// 5. Language hint: `VOICEPI_LANG` (shared with the local
    ///    backend so a user who changed it once picks it up for
    ///    both paths).
    /// 6. Initial prompt: `VOICEPI_INITIAL_PROMPT` (same rationale).
    ///
    /// Empty / whitespace-only strings collapse to `None` for the
    /// language + prompt so the HTTP layer never receives a literal
    /// empty field.
    pub fn from_env() -> Self {
        let base_url = env_trimmed(STT_BASE_URL_ENV)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let model = env_trimmed(STT_MODEL_ENV).unwrap_or_default();
        let api_key = resolve_api_key(&base_url);
        let timeout_ms = env_trimmed(STT_TIMEOUT_MS_ENV)
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.max(TIMEOUT_FLOOR_MS))
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let language = env_trimmed(LANG_ENV).filter(|value| !value.is_empty());
        let initial_prompt = env_trimmed(INITIAL_PROMPT_ENV).filter(|value| !value.is_empty());
        Self {
            base_url,
            api_key,
            model,
            language,
            initial_prompt,
            timeout_ms,
        }
    }
}

/// Resolve the API key from the same env-var precedence Python's
/// `vp_external_api._api_key` implements: `VOICEPI_STT_API_KEY`
/// first, then `GROQ_API_KEY` if the base URL looks like Groq, then
/// `OPENAI_API_KEY`. Returns an empty string when nothing is set --
/// the backend's `new` constructor treats that as an error so the
/// caller sees the failure at construction time.
fn resolve_api_key(base_url: &str) -> String {
    if let Some(key) = env_trimmed(STT_API_KEY_ENV).filter(|value| !value.is_empty()) {
        return key;
    }
    if base_url.to_ascii_lowercase().contains("api.groq.com") {
        if let Some(key) = env_trimmed(GROQ_API_KEY_ENV).filter(|value| !value.is_empty()) {
            return key;
        }
    }
    env_trimmed(OPENAI_API_KEY_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

/// Read `name` from the env, trim ASCII whitespace, and return the
/// owned string. Returns `None` when the variable is unset (leaves the
/// caller to decide between defaulting and erroring).
fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
}

/// Production [`TranscribeBackend`] that POSTs 16 kHz mono PCM to an
/// OpenAI-compatible transcription endpoint.
///
/// Cheap to construct: only the env resolution runs at `new` time; no
/// network activity until the first [`Self::transcribe`] call. Sends
/// each request synchronously (blocking the coordinator thread inside
/// [`crate::dictate::DictateSession::stop_and_transcribe`]) -- matches
/// the local backend's `TranscribeBackend::transcribe` contract, which
/// the session calls from a thread that is idle until the request
/// returns.
///
/// `Debug` is derived so tests using `Result::unwrap_err` on the
/// `new()` constructor produce a legible panic message; the wrapped
/// config already derives `Debug` so the impl is trivial.
#[derive(Debug)]
pub struct CloudTranscribeBackend {
    config: CloudBackendConfig,
}

impl CloudTranscribeBackend {
    /// Build a backend from a fully-resolved config. Rejects an empty
    /// API key, base URL, or model up-front so the supervisor's stderr
    /// event names the missing setting once at startup instead of
    /// every press producing a cryptic 401 / empty-body error.
    ///
    /// Language + prompt are optional, so no validation on those.
    pub fn new(config: CloudBackendConfig) -> Result<Self, String> {
        if config.base_url.trim().is_empty() {
            return Err(format!(
                "cloud STT base URL is empty (set {STT_BASE_URL_ENV}=…)"
            ));
        }
        if config.api_key.trim().is_empty() {
            return Err(format!(
                "cloud STT API key is empty (set {STT_API_KEY_ENV}=… or the provider-specific fallback)"
            ));
        }
        if config.model.trim().is_empty() {
            return Err(format!("cloud STT model is empty (set {STT_MODEL_ENV}=…)"));
        }
        Ok(Self { config })
    }

    /// Read-only view of the resolved config. Exposed for tests +
    /// startup diagnostics; production callers should not need to
    /// reach in.
    pub fn config(&self) -> &CloudBackendConfig {
        &self.config
    }
}

impl TranscribeBackend for CloudTranscribeBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        // Refuse a `sample_rate == 0` up-front: `hound::WavSpec` accepts
        // a zero rate at construction but the resulting WAV is unplayable
        // and `duration_s` below would divide by zero. The session pins
        // `sample_rate` to `SR = 16_000`, so the guard only fires on a
        // caller that bypassed the session (which is why it is worth an
        // explicit error rather than a silent 0).
        if sample_rate == 0 {
            return Err(TranscribeError::Backend(
                "cloud transcribe requires a non-zero sample_rate".to_owned(),
            ));
        }
        let wav_bytes = encode_pcm_as_wav(pcm, sample_rate)
            .map_err(|err| TranscribeError::Backend(format!("wav encode: {err}")))?;

        let start = std::time::Instant::now();
        let result = cloud_transcribe(
            &self.config.base_url,
            &self.config.api_key,
            &self.config.model,
            &wav_bytes,
            self.config.language.as_deref(),
            self.config.initial_prompt.as_deref(),
            self.config.timeout_ms,
        )
        .map_err(|err| TranscribeError::Backend(format!("{err:#}")))?;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        let duration_s = pcm.len() as f64 / f64::from(sample_rate);
        // The session takes the empty-text branch on an empty `text`,
        // so we do not have to gate anything specifically here --
        // whitespace-trimming is enough. `cloud_transcribe` already
        // trims the server response before handing it back.
        Ok(TranscribeResult {
            text: result.text,
            // No hallucination filter on cloud output today: the
            // provider models don't emit whisper.cpp's subtitle-credit
            // patterns, and the exact-blacklist filter is Whisper-specific.
            is_hallucination: false,
            latency_ms,
            duration_s,
            language: result
                .language
                .or_else(|| self.config.language.clone())
                .unwrap_or_default(),
            gate: None,
        })
    }
}

/// Encode `pcm` (mono, `sample_rate` Hz, samples in [-1.0, 1.0]) as a
/// 16-bit signed WAV blob in memory. Uses `hound::WavWriter` on a
/// `Cursor<Vec<u8>>` so the output can be POSTed straight into the
/// multipart body without touching disk.
///
/// Values outside [-1.0, 1.0] are clamped BEFORE the i16 scaling so a
/// too-loud sample cannot wrap the sign bit and invert the waveform.
pub(crate) fn encode_pcm_as_wav(pcm: &[f32], sample_rate: u32) -> Result<Vec<u8>, hound::Error> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(44 + pcm.len() * 2);
    {
        let mut writer = WavWriter::new(Cursor::new(&mut buf), spec)?;
        for &sample in pcm {
            // Clamp to the closed unit interval so 16-bit scaling never
            // overflows into the sign bit. Rounding via `round()`
            // matches the standard PCM-scaling convention (half-away
            // from zero).
            let clamped = sample.clamp(-1.0, 1.0);
            let scaled = (clamped * f32::from(i16::MAX)).round() as i16;
            writer.write_sample(scaled)?;
        }
        writer.finalize()?;
    }
    Ok(buf)
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
