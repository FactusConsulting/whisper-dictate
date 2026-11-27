//! Fixed loopback protocol for the separately started CUDA Hviske service.

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::Deserialize;

use super::CloudTranscriptionResult;

macro_rules! local_url {
    ($suffix:literal) => {
        concat!("http://127.0.0.1:18765", $suffix)
    };
}

pub(crate) const BASE_URL: &str = local_url!("");
const HEALTH_URL: &str = local_url!("/wd-hviske/v1/health");
const TRANSCRIBE_URL: &str = local_url!("/wd-hviske/v1/transcribe");
const MODEL_HEADER: &str = "X-WD-Hviske-Model";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const LANGUAGE: &str = "da";
pub(crate) const DEVICE: &str = "cuda";
pub(crate) const PROVIDER_LABEL: &str = "Hviske on this PC";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalModel {
    V53,
    V6,
}

impl LocalModel {
    pub(crate) fn from_alias(model: &str) -> Option<Self> {
        let model = model.trim();
        [Self::V53, Self::V6]
            .into_iter()
            .find(|candidate| model.eq_ignore_ascii_case(candidate.alias()))
    }

    pub(crate) const fn alias(self) -> &'static str {
        match self {
            Self::V53 => "local:syvai/hviske-v5.3",
            Self::V6 => "local:syvai/hviske-v6",
        }
    }

    pub(crate) const fn model_id(self) -> &'static str {
        match self {
            Self::V53 => super::HVISKE_V53,
            Self::V6 => super::HVISKE_V6,
        }
    }

    pub(crate) const fn required_mib(self) -> u32 {
        match self {
            Self::V53 => 6_144,
            Self::V6 => 3_584,
        }
    }
}

/// A providerless CLI request can also opt in through its explicit model ID.
pub(crate) fn require_custom_provider(provider: &str) -> Result<()> {
    let provider = provider.trim();
    ensure!(
        provider.is_empty() || provider.eq_ignore_ascii_case("custom"),
        "on-this-PC Hviske requires the Custom STT provider"
    );
    Ok(())
}

#[derive(Deserialize)]
struct Identity {
    model: String,
    device: String,
}

fn verify_identity(model: &str, device: &str, expected: LocalModel) -> Result<()> {
    ensure!(
        model == expected.model_id() && device == DEVICE,
        "expected model {} on CUDA, got model {model:?} on {device:?}",
        expected.model_id()
    );
    Ok(())
}

#[derive(Deserialize)]
struct Transcript {
    model: String,
    device: String,
    text: String,
}

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
});

pub(crate) fn ensure_ready(model: LocalModel) -> Result<()> {
    check_health(HEALTH_URL, model)
}

fn check_health(url: &str, model: LocalModel) -> Result<()> {
    let check = || -> Result<()> {
        let response = AGENT
            .get(url)
            .config()
            .timeout_global(Some(HEALTH_TIMEOUT))
            .build()
            .call()?;
        let identity: Identity = read_response(response)?;
        verify_identity(&identity.model, &identity.device, model)
    };
    check().with_context(|| {
        format!(
            "local Hviske is not ready at {BASE_URL}; start the service with {} on CUDA (requires at least {} MiB VRAM)",
            model.model_id(), model.required_mib()
        )
    })
}

pub(super) fn transcribe(
    model: LocalModel,
    wav: &[u8],
    timeout_ms: u64,
) -> Result<CloudTranscriptionResult> {
    transcribe_at(HEALTH_URL, TRANSCRIBE_URL, model, wav, timeout_ms)
}

fn transcribe_at(
    health_url: &str,
    transcribe_url: &str,
    model: LocalModel,
    wav: &[u8],
    timeout_ms: u64,
) -> Result<CloudTranscriptionResult> {
    let spec = hound::WavReader::new(wav)
        .context("local Hviske requires valid WAV audio")?
        .spec();
    ensure!(
        spec.channels == 1
            && spec.sample_rate == 16_000
            && spec.bits_per_sample == 16
            && spec.sample_format == hound::SampleFormat::Int,
        "local Hviske requires mono 16 kHz signed 16-bit PCM WAV audio"
    );
    check_health(health_url, model)?;
    let response = AGENT
        .post(transcribe_url)
        .header("Content-Type", "audio/wav")
        .header(MODEL_HEADER, model.model_id())
        .config()
        .timeout_global(Some(Duration::from_millis(timeout_ms.max(1_000))))
        .build()
        .send(wav)
        .context("local Hviske transcription request failed")?;
    let transcript: Transcript =
        read_response(response).context("local Hviske transcription failed")?;
    verify_identity(&transcript.model, &transcript.device, model)?;
    let mut text = transcript.text;
    text.truncate(text.trim_end().len());
    let leading = text.len() - text.trim_start().len();
    if leading > 0 {
        text.replace_range(..leading, "");
    }
    Ok(CloudTranscriptionResult {
        text,
        language: Some(LANGUAGE.to_owned()),
    })
}

fn read_response<T: serde::de::DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<T> {
    ensure!(
        response.status().is_success(),
        "unexpected HTTP status {}",
        response.status()
    );
    response
        .body_mut()
        .read_json()
        .context("invalid JSON response")
}

#[cfg(test)]
#[path = "hviske_local_tests.rs"]
mod tests;
