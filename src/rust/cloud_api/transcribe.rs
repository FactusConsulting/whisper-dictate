//! OpenAI-compatible `/audio/transcriptions` client (Whisper / GPT-4o-mini-transcribe / Groq).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::cloud_api::http::{check_status, http_error, USER_AGENT};

pub const GROQ_TRANSCRIPTION_PROMPT_LIMIT: usize = 896;

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
    let mut response = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(Duration::from_millis(timeout_ms.max(1000))))
        .http_status_as_error(false)
        .build()
        .send(&body[..])
        .map_err(|err| anyhow!("cloud transcription failed: {}", http_error(err)))?;
    check_status(&mut response)
        .map_err(|detail| anyhow!("cloud transcription failed: {detail}"))?;
    let body: Value = response
        .body_mut()
        .read_json()
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

pub(crate) fn cap_transcription_prompt<'a>(prompt: &'a str, base_url: &str) -> &'a str {
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

#[cfg(test)]
mod tests {
    use super::*;

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
