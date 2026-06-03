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
            let detail = response.into_string().unwrap_or_default();
            if detail.trim().is_empty() {
                format!("HTTP {code}")
            } else {
                format!("HTTP {code}: {}", detail.trim())
            }
        }
        other => other.to_string(),
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
