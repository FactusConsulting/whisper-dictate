use anyhow::Result;
use std::env;

use crate::config::AppSettings;

pub(super) const GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub(super) const GROQ_STT_MODEL: &str = "whisper-large-v3-turbo";
pub(super) const GROQ_POST_MODEL: &str = "llama-3.1-8b-instant";
pub(super) const GROQ_KEYS_URL: &str = "https://console.groq.com/keys";
pub(super) const OPENAI_STT_BASE_URL: &str = "https://api.openai.com/v1";
pub(super) const OPENAI_STT_MODEL: &str = "gpt-4o-mini-transcribe";
pub(super) const OPENAI_POST_MODEL: &str = "gpt-4o-mini";
pub(super) const OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys";
pub(super) const STT_API_KEY_ENV: &str = "VOICEPI_STT_API_KEY";
pub(super) const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY";

const CREDENTIAL_SERVICE: &str = "whisper-dictate";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloudProvider {
    Groq,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostProvider {
    Groq,
    OpenAi,
}

impl CloudProvider {
    pub(super) fn from_raw(raw: &str) -> Option<Self> {
        match raw {
            "groq" => Some(Self::Groq),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    pub(super) fn from_settings(settings: &AppSettings) -> Self {
        if let Some(provider) = Self::from_raw(settings.stt_provider.trim()) {
            return provider;
        }
        if settings
            .stt_base_url
            .to_ascii_lowercase()
            .contains("api.groq.com")
        {
            Self::Groq
        } else {
            Self::OpenAi
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Groq => "groq",
            Self::OpenAi => "openai",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq",
            Self::OpenAi => "OpenAI",
        }
    }

    pub(super) fn base_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_STT_BASE_URL,
            Self::OpenAi => OPENAI_STT_BASE_URL,
        }
    }

    pub(super) fn default_model(self) -> &'static str {
        match self {
            Self::Groq => GROQ_STT_MODEL,
            Self::OpenAi => OPENAI_STT_MODEL,
        }
    }

    pub(super) fn model_options(self) -> &'static [&'static str] {
        match self {
            Self::Groq => super::GROQ_STT_MODELS,
            Self::OpenAi => super::OPENAI_STT_MODELS,
        }
    }

    pub(super) fn key_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_KEYS_URL,
            Self::OpenAi => OPENAI_KEYS_URL,
        }
    }

    fn credential_user(self) -> &'static str {
        match self {
            Self::Groq => "stt-api-key:groq",
            Self::OpenAi => "stt-api-key:openai",
        }
    }
}

impl PostProvider {
    pub(super) fn from_settings(settings: &AppSettings) -> Option<Self> {
        match settings.post_processor.as_str() {
            "groq" => Some(Self::Groq),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq post-processing",
            Self::OpenAi => "OpenAI post-processing",
        }
    }

    pub(super) fn key_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_KEYS_URL,
            Self::OpenAi => OPENAI_KEYS_URL,
        }
    }

    fn credential_user(self) -> &'static str {
        match self {
            Self::Groq => "post-api-key:groq",
            Self::OpenAi => "post-api-key:openai",
        }
    }
}

pub(super) fn load_stt_api_key_state(provider: CloudProvider) -> Result<(String, String, String)> {
    let key = load_stt_api_key(provider)?;
    if !key.is_empty() {
        let status = format!(
            "Loaded saved {} API key from credential store.",
            provider.label()
        );
        return Ok((key.clone(), key, status));
    }

    if let Some(env_key) = load_stt_api_key_from_env(provider) {
        let status = format!(
            "Loaded {} API key from environment. Use Save API key to store it.",
            provider.label()
        );
        return Ok((env_key.clone(), env_key, status));
    }

    Ok((
        String::new(),
        String::new(),
        format!("No {} API key saved.", provider.label()),
    ))
}

pub(super) fn load_post_api_key_state(
    provider: Option<PostProvider>,
) -> Result<(String, String, String)> {
    let Some(provider) = provider else {
        return Ok((
            String::new(),
            String::new(),
            "Post processor is local or disabled; no post API key needed.".to_owned(),
        ));
    };
    let key = load_post_api_key(provider)?;
    if !key.is_empty() {
        let status = format!(
            "Loaded saved {} API key from credential store.",
            provider.label()
        );
        return Ok((key.clone(), key, status));
    }

    if let Some(env_key) = load_post_api_key_from_env(provider) {
        let status = format!(
            "Loaded {} API key from environment. Save post API key to store it.",
            provider.label()
        );
        return Ok((env_key.clone(), env_key, status));
    }

    Ok((
        String::new(),
        String::new(),
        format!(
            "No {} API key saved. The worker can fall back to the Cloud STT key if one is loaded.",
            provider.label()
        ),
    ))
}

pub(super) fn load_stt_api_key_from_env(provider: CloudProvider) -> Option<String> {
    let candidates: &[&str] = match provider {
        CloudProvider::Groq => &["VOICEPI_STT_API_KEY", "GROQ_API_KEY"],
        CloudProvider::OpenAi => &["VOICEPI_STT_API_KEY", "OPENAI_API_KEY"],
    };
    candidates
        .iter()
        .filter_map(|name| env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
}

pub(super) fn load_post_api_key_from_env(provider: PostProvider) -> Option<String> {
    let candidates: &[&str] = match provider {
        PostProvider::Groq => &[
            "VOICEPI_POST_API_KEY",
            "VOICEPI_STT_API_KEY",
            "GROQ_API_KEY",
        ],
        PostProvider::OpenAi => &[
            "VOICEPI_POST_API_KEY",
            "VOICEPI_STT_API_KEY",
            "OPENAI_API_KEY",
        ],
    };
    candidates
        .iter()
        .filter_map(|name| env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
}

pub(super) fn save_stt_api_key(provider: CloudProvider, secret: &str) -> Result<()> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, provider.credential_user())?;
    if secret.trim().is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    } else {
        entry.set_password(secret.trim())?;
        Ok(())
    }
}

pub(super) fn save_post_api_key(provider: PostProvider, secret: &str) -> Result<()> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, provider.credential_user())?;
    if secret.trim().is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    } else {
        entry.set_password(secret.trim())?;
        Ok(())
    }
}

fn load_stt_api_key(provider: CloudProvider) -> Result<String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, provider.credential_user())?;
    match entry.get_password() {
        Ok(secret) => Ok(secret),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(err) => Err(err.into()),
    }
}

fn load_post_api_key(provider: PostProvider) -> Result<String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, provider.credential_user())?;
    match entry.get_password() {
        Ok(secret) => Ok(secret),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(err) => Err(err.into()),
    }
}
