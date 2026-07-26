//! OpenAI-compatible `/audio/transcriptions` client (Whisper / GPT-4o-mini-transcribe / Groq).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::cloud_api::http::{check_status, http_error, platform_tls_agent, USER_AGENT};

pub const GROQ_TRANSCRIPTION_PROMPT_LIMIT: usize = 896;

/// Resolve the STT API key without requiring it on the command line.
///
/// `--api-key` used to be mandatory, which meant every caller had to put the
/// secret in `argv` — and a process's command line is readable by other local
/// users (`ps aux`, `/proc/<pid>/cmdline`; Linux only restricts it when
/// `hidepid` is mounted, which is not the default). The environment block is
/// not: `/proc/<pid>/environ` is owner-only. So callers pass the key in the
/// child's env instead, and an explicit `--api-key` stays supported for
/// backwards compatibility.
///
/// Precedence mirrors `dictate::backends::cloud_transcribe::from_env_with`
/// and `ui/api_keys.rs::load_stt_api_key_from_env`: the explicit flag first,
/// then the STT-specific var, then ONLY the generic var for the provider
/// implied by `base_url` (so a stray `OPENAI_API_KEY` is never sent to Groq,
/// or vice versa).
///
/// Takes `lookup` rather than reading process env directly so the precedence
/// is unit-testable without mutating global state.
pub fn resolve_api_key_with(
    flag: &str,
    base_url: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> String {
    let flag = flag.trim();
    if !flag.is_empty() {
        return flag.to_owned();
    }
    let get = |name: &str| {
        lookup(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let generic = if base_url.to_ascii_lowercase().contains("groq.com") {
        "GROQ_API_KEY"
    } else {
        "OPENAI_API_KEY"
    };
    get("VOICEPI_STT_API_KEY")
        .or_else(|| get(generic))
        .unwrap_or_default()
}

/// Process-env wrapper around [`resolve_api_key_with`].
pub fn resolve_api_key(flag: &str, base_url: &str) -> String {
    resolve_api_key_with(flag, base_url, |name| std::env::var(name).ok())
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
    let mut response = platform_tls_agent()
        .post(&url)
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
    // Walk backward from the byte limit to the nearest char boundary so we
    // never split a multi-byte UTF-8 sequence.
    let end = (0..=GROQ_TRANSCRIPTION_PROMPT_LIMIT.min(prompt.len()))
        .rev()
        .find(|&i| prompt.is_char_boundary(i))
        .unwrap_or(0);
    prompt[..end].trim_end()
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

    /// Build a `lookup` over a fixed table so the precedence is exercised
    /// without mutating process env (which would race other tests).
    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name: &str| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    const OPENAI: &str = "https://api.openai.com/v1";
    const GROQ: &str = "https://api.groq.com/openai/v1";

    #[test]
    fn explicit_flag_wins_over_env() {
        let key = resolve_api_key_with(
            "flag-key",
            OPENAI,
            lookup_from(&[("VOICEPI_STT_API_KEY", "env-key")]),
        );
        assert_eq!(key, "flag-key");
    }

    #[test]
    fn falls_back_to_stt_specific_env_when_flag_absent() {
        let key = resolve_api_key_with(
            "",
            OPENAI,
            lookup_from(&[("VOICEPI_STT_API_KEY", "env-key")]),
        );
        assert_eq!(key, "env-key");
    }

    #[test]
    fn blank_flag_is_treated_as_absent() {
        // A caller that passes `--api-key ""` (or whitespace) must not end up
        // sending an empty Authorization header when the env has a real key.
        let key = resolve_api_key_with("   ", GROQ, lookup_from(&[("GROQ_API_KEY", "groq-key")]));
        assert_eq!(key, "groq-key");
    }

    #[test]
    fn generic_env_is_provider_scoped_by_base_url() {
        // Groq base URL -> GROQ_API_KEY, and an OpenAI key present in the
        // environment is NOT sent to Groq (and vice versa). This is the same
        // rule `cloud_transcribe::from_env_with` applies; a leak here would
        // hand one provider's secret to another.
        let groq = resolve_api_key_with(
            "",
            GROQ,
            lookup_from(&[
                ("GROQ_API_KEY", "groq-key"),
                ("OPENAI_API_KEY", "openai-key"),
            ]),
        );
        assert_eq!(groq, "groq-key");

        let openai = resolve_api_key_with(
            "",
            OPENAI,
            lookup_from(&[
                ("GROQ_API_KEY", "groq-key"),
                ("OPENAI_API_KEY", "openai-key"),
            ]),
        );
        assert_eq!(openai, "openai-key");

        // Only the wrong-provider key is set: resolve to empty rather than
        // sending it. `cloud_transcribe` then fails with a clear message.
        let mismatched =
            resolve_api_key_with("", GROQ, lookup_from(&[("OPENAI_API_KEY", "openai-key")]));
        assert_eq!(mismatched, "");
    }

    #[test]
    fn stt_specific_env_outranks_the_generic_one() {
        let key = resolve_api_key_with(
            "",
            GROQ,
            lookup_from(&[
                ("VOICEPI_STT_API_KEY", "stt-key"),
                ("GROQ_API_KEY", "groq-key"),
            ]),
        );
        assert_eq!(key, "stt-key");
    }

    #[test]
    fn empty_when_nothing_is_set() {
        assert_eq!(resolve_api_key_with("", OPENAI, lookup_from(&[])), "");
    }

    #[test]
    fn whitespace_only_env_values_are_ignored() {
        let key = resolve_api_key_with(
            "",
            OPENAI,
            lookup_from(&[
                ("VOICEPI_STT_API_KEY", "  "),
                ("OPENAI_API_KEY", "openai-key"),
            ]),
        );
        assert_eq!(key, "openai-key");
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

    #[test]
    fn groq_cap_does_not_split_multibyte_chars() {
        // Build a prompt where the limit byte lands inside a 3-byte UTF-8
        // sequence (U+4E2D "中" = 0xE4 0xB8 0xAD). Fill with ASCII up to
        // LIMIT-2, then append "中" so bytes LIMIT-2..LIMIT+1 form the
        // codepoint. The cap must step back to LIMIT-2, not panic.
        let ascii_part = "a".repeat(GROQ_TRANSCRIPTION_PROMPT_LIMIT - 2);
        let prompt = format!("{ascii_part}中中中");
        assert!(prompt.len() > GROQ_TRANSCRIPTION_PROMPT_LIMIT);

        let capped = cap_transcription_prompt(&prompt, "https://api.groq.com/openai/v1");
        // Result must be valid UTF-8 and at most LIMIT bytes.
        assert!(capped.len() <= GROQ_TRANSCRIPTION_PROMPT_LIMIT);
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());
    }
}
