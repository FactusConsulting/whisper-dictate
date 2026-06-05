use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::config::AppSettings;

const USER_AGENT: &str =
    "whisper-dictate/0.3 (+https://github.com/FactusConsulting/whisper-dictate)";
const GROQ_TRANSCRIPTION_PROMPT_LIMIT: usize = 896;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloudTranscriptionResult {
    pub text: String,
    pub language: Option<String>,
}

pub fn handle_cloud_transcribe(
    base_url: &str,
    api_key: &str,
    model: &str,
    audio_wav_path: &Path,
    language: Option<&str>,
    prompt: Option<&str>,
    timeout_ms: u64,
) -> Result<()> {
    let result = cloud_transcribe(
        base_url,
        api_key,
        model,
        &std::fs::read(audio_wav_path)?,
        language,
        prompt,
        timeout_ms,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn cloud_transcribe(
    base_url: &str,
    api_key: &str,
    model: &str,
    audio_wav: &[u8],
    language: Option<&str>,
    prompt: Option<&str>,
    timeout_ms: u64,
) -> Result<CloudTranscriptionResult> {
    if api_key.trim().is_empty() {
        return Err(anyhow!("cloud transcription API key is empty"));
    }
    if model.trim().is_empty() {
        return Err(anyhow!("cloud transcription model is empty"));
    }

    let base_url = base_url.trim_end_matches('/');
    let mut fields = vec![("model", model.to_owned())];
    if let Some(language) = language.map(str::trim).filter(|value| !value.is_empty()) {
        fields.push(("language", language.to_owned()));
    }
    if let Some(prompt) = prompt.map(str::trim).filter(|value| !value.is_empty()) {
        fields.push((
            "prompt",
            cap_transcription_prompt(prompt, base_url).to_owned(),
        ));
    }
    let (body, boundary) = multipart_audio_body(&fields, audio_wav);
    let url = format!("{base_url}/audio/transcriptions");
    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_millis(timeout_ms.max(1000)))
        .send_bytes(&body)
        .map_err(|err| anyhow!("cloud transcription failed: {}", http_error(err)))?;
    let body: Value = response
        .into_json()
        .map_err(|err| anyhow!("cloud transcription returned invalid JSON: {err}"))?;
    Ok(CloudTranscriptionResult {
        text: body
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        language: body
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn cap_transcription_prompt<'a>(prompt: &'a str, base_url: &str) -> &'a str {
    if !base_url.to_ascii_lowercase().contains("api.groq.com")
        || prompt.len() <= GROQ_TRANSCRIPTION_PROMPT_LIMIT
    {
        return prompt;
    }
    prompt[..GROQ_TRANSCRIPTION_PROMPT_LIMIT].trim_end()
}

fn multipart_audio_body(fields: &[(&str, String)], audio_wav: &[u8]) -> (Vec<u8>, String) {
    let boundary = format!(
        "----whisper-dictate-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(audio_wav);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (body, boundary)
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

    #[test]
    fn multipart_audio_body_contains_model_language_and_file() {
        let (body, boundary) = multipart_audio_body(
            &[
                ("model", "gpt-4o-mini-transcribe".to_owned()),
                ("language", "da".to_owned()),
            ],
            b"RIFF....WAVE",
        );
        let body = String::from_utf8_lossy(&body);

        assert!(body.contains(&format!("--{boundary}")));
        assert!(body.contains("name=\"model\""));
        assert!(body.contains("gpt-4o-mini-transcribe"));
        assert!(body.contains("name=\"language\""));
        assert!(body.contains("filename=\"audio.wav\""));
        assert!(body.contains("Content-Type: audio/wav"));
    }

    #[test]
    fn groq_transcription_prompt_is_capped() {
        let prompt = "x".repeat(GROQ_TRANSCRIPTION_PROMPT_LIMIT + 20);

        assert_eq!(
            cap_transcription_prompt(&prompt, "https://api.groq.com/openai/v1").len(),
            GROQ_TRANSCRIPTION_PROMPT_LIMIT
        );
        assert_eq!(
            cap_transcription_prompt(&prompt, "https://api.openai.com/v1").len(),
            GROQ_TRANSCRIPTION_PROMPT_LIMIT + 20
        );
    }
}
