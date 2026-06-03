use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::config::AppSettings;

const USER_AGENT: &str =
    "whisper-dictate/0.3 (+https://github.com/FactusConsulting/whisper-dictate)";

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
        if api_key.is_empty() {
            return Err(anyhow!("cloud API key is empty"));
        }
        let provider = if settings.stt_provider.trim().eq_ignore_ascii_case("groq")
            || settings
                .stt_base_url
                .to_ascii_lowercase()
                .contains("api.groq.com")
        {
            "Groq"
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
            timeout_ms: parse_timeout_ms(&settings.post_timeout_ms, 2_000),
        })
    }
}

pub fn check_cloud_api(check: &CloudApiCheck) -> Result<CloudApiCheckResult> {
    let url = format!("{}/models", check.base_url.trim_end_matches('/'));
    let response = ureq::get(&url)
        .set("Authorization", &format!("Bearer {}", check.api_key))
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_millis(check.timeout_ms.max(1000)))
        .call()
        .map_err(|err| anyhow!("{} API check failed: {}", check.provider, http_error(err)))?;

    let body: Value = response
        .into_json()
        .map_err(|err| anyhow!("{} API returned invalid JSON: {err}", check.provider))?;
    let ids = model_ids(&body);
    Ok(CloudApiCheckResult {
        provider: check.provider.clone(),
        model: check.model.clone(),
        model_count: ids.len(),
        model_available: ids.iter().any(|id| id == &check.model),
    })
}

pub fn check_post_api(check: &PostApiCheck) -> Result<PostApiCheckResult> {
    let url = format!("{}/chat/completions", check.base_url.trim_end_matches('/'));
    let payload = serde_json::json!({
        "model": check.model,
        "messages": [
            {"role": "system", "content": "You rewrite dictated text faithfully."},
            {"role": "user", "content": format!(
                "Mode: {}\nReturn only this exact text with punctuation fixed: this is a post processing api test",
                check.mode
            )},
        ],
        "temperature": 0,
    });
    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", check.api_key))
        .set("Content-Type", "application/json")
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_millis(check.timeout_ms.max(1000)))
        .send_json(payload)
        .map_err(|err| {
            anyhow!(
                "{} post API check failed: {}",
                check.provider,
                http_error(err)
            )
        })?;

    let body: Value = response
        .into_json()
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

fn parse_timeout_ms(raw: &str, default: u64) -> u64 {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value >= 100)
        .unwrap_or(default)
}

fn http_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, response) => {
            let retry_after = response.header("Retry-After").map(str::to_owned);
            let detail = response.into_string().unwrap_or_default();
            if code == 429 {
                return rate_limit_message(retry_after.as_deref(), &detail);
            }
            if detail.trim().is_empty() {
                format!("HTTP {code}")
            } else {
                format!("HTTP {code}: {}", detail.trim())
            }
        }
        other => other.to_string(),
    }
}

fn rate_limit_message(retry_after: Option<&str>, detail: &str) -> String {
    let mut message = "HTTP 429 Too Many Requests: rate limited by provider".to_owned();
    if let Some(seconds) = retry_after.filter(|value| !value.trim().is_empty()) {
        message.push_str(&format!(" (retry after {}s)", seconds.trim()));
    }
    if !detail.trim().is_empty() {
        message.push_str(&format!(": {}", detail.trim()));
    }
    message
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
    fn rate_limit_message_includes_retry_after_and_detail() {
        let message = rate_limit_message(Some(" 12 "), r#"{"error":"rate limit"}"#);

        assert!(message.contains("HTTP 429 Too Many Requests"));
        assert!(message.contains("rate limited"));
        assert!(message.contains("retry after 12s"));
        assert!(message.contains("rate limit"));
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
}
