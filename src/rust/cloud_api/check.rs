//! Cloud "API reachable + model listed" checks used by the desktop UI.
//!
//! These hit the `/models` (transcription provider) and `/chat/completions`
//! (post-processing provider) endpoints with a probe payload to validate the
//! configured API key, base URL and model id before the user records anything.
//! Nemotron's hosted Riva gRPC endpoint is routed through its config RPC rather
//! than receiving an HTTP `/models` request.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::grpc;
use crate::cloud_api::http::{
    check_status, http_error, parse_timeout_ms, platform_tls_agent, USER_AGENT,
};
use crate::cloud_api::prompts::POSTPROCESS_SYSTEM_PROMPT;
use crate::config::AppSettings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudApiCheck {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudApiCheckResult {
    pub provider: String,
    pub model: String,
    pub model_count: usize,
    pub model_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostApiCheck {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub mode: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostApiCheckResult {
    pub provider: String,
    pub model: String,
    pub output: String,
}

impl PostApiCheckResult {
    pub fn summary(&self) -> String {
        format!(
            "{} post-processing API reachable; model {} returned: {}",
            self.provider, self.model, self.output
        )
    }
}

impl CloudApiCheckResult {
    pub fn summary(&self) -> String {
        if self.model_available {
            format!(
                "{} API reachable; model {} is available ({} models).",
                self.provider, self.model, self.model_count
            )
        } else {
            format!(
                "{} API reachable, but model {} was not listed ({} models).",
                self.provider, self.model, self.model_count
            )
        }
    }
}

impl CloudApiCheck {
    pub fn from_settings(settings: &AppSettings, api_key: &str) -> Result<Self> {
        if settings.stt_backend != "openai" {
            return Err(anyhow!("cloud API check requires STT backend = openai"));
        }
        let api_key = api_key.trim();
        let loopback = crate::privacy::is_loopback_url(settings.stt_base_url.trim());
        if api_key.is_empty() && !loopback {
            return Err(anyhow!("cloud API key is empty"));
        }
        let provider = if settings.stt_provider.trim().eq_ignore_ascii_case("groq")
            || settings
                .stt_base_url
                .to_ascii_lowercase()
                .contains("api.groq.com")
        {
            "Groq"
        } else if settings
            .stt_provider
            .trim()
            .eq_ignore_ascii_case("nemotron")
        {
            "Nemotron 3.5 ASR"
        } else {
            "OpenAI"
        };
        let model = settings.stt_model.trim();
        if model.is_empty() {
            return Err(anyhow!("cloud STT model is empty"));
        }
        Ok(Self {
            provider: provider.to_owned(),
            base_url: settings.stt_base_url.trim_end_matches('/').to_owned(),
            model: model.to_owned(),
            api_key: api_key.to_owned(),
            timeout_ms: parse_timeout_ms(&settings.stt_timeout_ms, 30_000),
        })
    }

    /// Whether this check targets a Riva gRPC Nemotron endpoint instead of
    /// the usual OpenAI-compatible `/models` endpoint.
    pub fn uses_grpc(&self) -> bool {
        grpc::is_nemotron_grpc_endpoint(&self.provider, &self.base_url)
    }

    /// Human-readable operation shown in the UI task log and completion row.
    pub fn operation(&self) -> String {
        if self.uses_grpc() {
            format!("{} gRPC GetRivaSpeechRecognitionConfig", self.provider)
        } else {
            format!("{} /models", self.provider)
        }
    }
}

impl PostApiCheck {
    pub fn from_settings(settings: &AppSettings, api_key: &str) -> Result<Self> {
        let processor = settings.post_processor.trim();
        if !matches!(processor, "openai" | "groq") {
            return Err(anyhow!(
                "post API check requires Post processor = groq or openai"
            ));
        }
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(anyhow!("post API key is empty"));
        }
        let model = settings.post_model.trim();
        if model.is_empty() {
            return Err(anyhow!("post model is empty"));
        }
        let provider = if processor == "groq" {
            "Groq"
        } else {
            "OpenAI"
        };
        Ok(Self {
            provider: provider.to_owned(),
            base_url: settings.post_base_url.trim_end_matches('/').to_owned(),
            model: model.to_owned(),
            api_key: api_key.to_owned(),
            mode: settings.post_mode.trim().to_owned(),
            timeout_ms: parse_timeout_ms(&settings.post_timeout_ms, 4_000),
        })
    }
}

pub fn check_cloud_api(check: &CloudApiCheck) -> Result<CloudApiCheckResult> {
    if check.uses_grpc() {
        let ids = grpc::check_nemotron_grpc(&check.base_url, &check.api_key, check.timeout_ms)?;
        // Riva's config RPC may return an empty list when the endpoint's
        // function has already selected the only model. A successful RPC is
        // still a valid reachability/model probe in that case. When names are
        // returned, accept exact IDs plus the hosted/local Nemotron naming
        // variants (`nvidia/...` versus `nemotron-asr-streaming`).
        let model_available =
            ids.is_empty() || ids.iter().any(|id| model_id_matches(&check.model, id));
        return Ok(CloudApiCheckResult {
            provider: check.provider.clone(),
            model: check.model.clone(),
            model_count: ids.len(),
            model_available,
        });
    }
    let url = format!("{}/models", check.base_url.trim_end_matches('/'));
    let mut request = platform_tls_agent().get(&url);
    if !check.api_key.is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", check.api_key));
    }
    let mut response = request
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(Duration::from_millis(check.timeout_ms.max(1000))))
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|err| anyhow!("{} API check failed: {}", check.provider, http_error(err)))?;
    check_status(&mut response)
        .map_err(|detail| anyhow!("{} API check failed: {detail}", check.provider))?;

    let body: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| anyhow!("{} API returned invalid JSON: {err}", check.provider))?;
    let ids = model_ids(&body);
    Ok(CloudApiCheckResult {
        provider: check.provider.clone(),
        model: check.model.clone(),
        model_count: ids.len(),
        // OpenAI-compatible NIM endpoints may advertise the short service
        // name (`nemotron-asr-streaming`) even when the request uses the
        // full profile id from the Speech-tab picker. Keep the same
        // profile-aware alias matching used by the gRPC probe.
        model_available: ids.iter().any(|id| model_id_matches(&check.model, id)),
    })
}

pub fn check_post_api(check: &PostApiCheck) -> Result<PostApiCheckResult> {
    let url = format!("{}/chat/completions", check.base_url.trim_end_matches('/'));
    let payload = serde_json::json!({
        "model": check.model,
        "messages": [
            {"role": "system", "content": POSTPROCESS_SYSTEM_PROMPT},
            {"role": "user", "content": format!(
                "Mode: {}\nReturn only this exact text with punctuation fixed: this is a post processing api test",
                check.mode
            )},
        ],
        "temperature": 0,
    });
    let mut response = platform_tls_agent()
        .post(&url)
        .header("Authorization", &format!("Bearer {}", check.api_key))
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(Duration::from_millis(check.timeout_ms.max(1000))))
        .http_status_as_error(false)
        .build()
        .send_json(payload)
        .map_err(|err| {
            anyhow!(
                "{} post API check failed: {}",
                check.provider,
                http_error(err)
            )
        })?;
    check_status(&mut response)
        .map_err(|detail| anyhow!("{} post API check failed: {detail}", check.provider))?;

    let body: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| anyhow!("{} post API returned invalid JSON: {err}", check.provider))?;
    let output = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| anyhow!("{} post API returned no message content", check.provider))?;
    Ok(PostApiCheckResult {
        provider: check.provider.clone(),
        model: check.model.clone(),
        output: output.to_owned(),
    })
}

fn model_ids(value: &Value) -> Vec<String> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn model_id_matches(expected: &str, advertised: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    let advertised = advertised.trim().to_ascii_lowercase();
    expected == advertised
        || matches!(
            (
                nemotron_model_profile(&expected),
                nemotron_model_profile(&advertised)
            ),
            (Some(NemotronModelProfile::Generic), Some(_))
                | (Some(_), Some(NemotronModelProfile::Generic))
                | (
                    Some(NemotronModelProfile::English),
                    Some(NemotronModelProfile::English)
                )
                | (
                    Some(NemotronModelProfile::Multilingual),
                    Some(NemotronModelProfile::Multilingual)
                )
        )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NemotronModelProfile {
    Generic,
    English,
    Multilingual,
}

fn nemotron_model_profile(model: &str) -> Option<NemotronModelProfile> {
    match model {
        "nemotron-asr-streaming" | "nvidia/nemotron-asr-streaming" => {
            Some(NemotronModelProfile::Generic)
        }
        "nemotron-speech-streaming-en-0.6b" | "nvidia/nemotron-speech-streaming-en-0.6b" => {
            Some(NemotronModelProfile::English)
        }
        "nemotron-3.5-asr-streaming-0.6b" | "nvidia/nemotron-3.5-asr-streaming-0.6b" => {
            Some(NemotronModelProfile::Multilingual)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_check_from_settings_rejects_empty_key() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
            stt_model: "whisper-large-v3-turbo".to_owned(),
            ..AppSettings::default()
        };

        let err = CloudApiCheck::from_settings(&settings, " ").unwrap_err();

        assert!(err.to_string().contains("API key is empty"));
    }

    #[test]
    fn cloud_check_allows_keyless_loopback_nemotron() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            stt_base_url: "http://localhost:9000/v1".to_owned(),
            stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
            ..AppSettings::default()
        };
        let check = CloudApiCheck::from_settings(&settings, "").unwrap();
        assert_eq!(check.provider, "Nemotron 3.5 ASR");
    }

    #[test]
    fn cloud_check_uses_saved_provider_when_url_is_stale() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "groq".to_owned(),
            stt_base_url: "https://api.openai.com/v1".to_owned(),
            stt_model: "whisper-large-v3-turbo".to_owned(),
            ..AppSettings::default()
        };

        let check = CloudApiCheck::from_settings(&settings, "test-key").unwrap();

        assert_eq!(check.provider, "Groq");
    }

    #[test]
    fn post_check_from_settings_requires_cloud_post_processor() {
        let settings = AppSettings {
            post_processor: "ollama".to_owned(),
            post_model: "qwen2.5:3b".to_owned(),
            ..AppSettings::default()
        };

        let err = PostApiCheck::from_settings(&settings, "test-key").unwrap_err();

        assert!(err.to_string().contains("requires Post processor"));
    }

    #[test]
    fn post_check_from_settings_uses_post_config() {
        let settings = AppSettings {
            post_processor: "groq".to_owned(),
            post_model: "llama-3.1-8b-instant".to_owned(),
            post_base_url: "https://api.groq.com/openai/v1/".to_owned(),
            post_mode: "clean".to_owned(),
            post_timeout_ms: "3000".to_owned(),
            ..AppSettings::default()
        };

        let check = PostApiCheck::from_settings(&settings, "test-key").unwrap();

        assert_eq!(check.provider, "Groq");
        assert_eq!(check.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(check.model, "llama-3.1-8b-instant");
        assert_eq!(check.mode, "clean");
        assert_eq!(check.timeout_ms, 3000);
    }

    #[test]
    fn cloud_result_summary_reports_missing_model_without_failing() {
        let result = CloudApiCheckResult {
            provider: "Groq".to_owned(),
            model: "missing-model".to_owned(),
            model_count: 16,
            model_available: false,
        };

        assert!(result.summary().contains("was not listed"));
    }

    #[test]
    fn cloud_check_identifies_hosted_nemotron_grpc_operation() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            stt_base_url: "https://grpc.nvcf.nvidia.com:443".to_owned(),
            stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
            ..AppSettings::default()
        };

        let check = CloudApiCheck::from_settings(&settings, "test-key").unwrap();

        assert!(check.uses_grpc());
        assert!(check.operation().contains("GetRivaSpeechRecognitionConfig"));
    }

    #[test]
    fn grpc_model_matching_accepts_hosted_nemotron_name() {
        assert!(model_id_matches(
            "nvidia/nemotron-3.5-asr-streaming-0.6b",
            "nemotron-asr-streaming"
        ));
        assert!(!model_id_matches(
            "whisper-large-v3",
            "nemotron-asr-streaming"
        ));
        assert!(!model_id_matches(
            "nvidia/nemotron-3.5-asr-streaming-0.6b",
            "nemotron-4-asr-streaming"
        ));
        assert!(!model_id_matches(
            "tenant/nemotron-3.5-asr-streaming-0.6b",
            "other/nemotron-3.5-asr-streaming-0.6b"
        ));
        assert!(model_id_matches(
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "nvidia/nemotron-speech-streaming-en-0.6b"
        ));
        assert!(!model_id_matches(
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "nvidia/nemotron-3.5-asr-streaming-0.6b"
        ));
        assert!(model_id_matches(
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "nemotron-asr-streaming"
        ));
    }
}
