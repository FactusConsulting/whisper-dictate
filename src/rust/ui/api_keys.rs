//! Cloud/post-processing provider model and the high-level API-key load/save
//! orchestration. The OS keyring is the primary store; the low-level fallback
//! key-file primitives and report types live in `super::secret_store`.

use anyhow::{Context, Result};
use keyring_core::{Entry, Error};
use std::env;
use std::sync::OnceLock;

use super::secret_store::*;
use crate::config::AppSettings;

pub(super) const GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub(super) const GROQ_STT_MODEL: &str = "whisper-large-v3-turbo";
pub(super) const GROQ_POST_MODEL: &str = "llama-3.3-70b-versatile";
pub(super) const GROQ_KEYS_URL: &str = "https://console.groq.com/keys";
pub(super) const OPENAI_STT_BASE_URL: &str = "https://api.openai.com/v1";
pub(super) const OPENAI_STT_MODEL: &str = "gpt-4o-mini-transcribe";
pub(super) const OPENAI_POST_MODEL: &str = "gpt-4o-mini";
pub(super) const OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys";
pub(super) const STT_API_KEY_ENV: &str = "VOICEPI_STT_API_KEY";
pub(super) const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY";

const CREDENTIAL_SERVICE: &str = "whisper-dictate";

pub(super) const CUSTOM_STT_BASE_URL: &str = "http://localhost:8000/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloudProvider {
    Groq,
    OpenAi,
    /// Self-hosted OpenAI-compatible endpoint (e.g. a local faster-whisper
    /// container). The base URL and model are user-managed and never normalized.
    Custom,
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
            "custom" => Some(Self::Custom),
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
            Self::Custom => "custom",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq",
            Self::OpenAi => "OpenAI",
            Self::Custom => "Custom (OpenAI-compatible)",
        }
    }

    pub(super) fn base_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_STT_BASE_URL,
            Self::OpenAi => OPENAI_STT_BASE_URL,
            // Seed for a fresh self-hosted setup; the user edits it freely and it
            // is never normalized back.
            Self::Custom => CUSTOM_STT_BASE_URL,
        }
    }

    pub(super) fn default_model(self) -> &'static str {
        match self {
            Self::Groq => GROQ_STT_MODEL,
            Self::OpenAi => OPENAI_STT_MODEL,
            Self::Custom => "",
        }
    }

    pub(super) fn model_options(self) -> &'static [&'static str] {
        match self {
            Self::Groq => super::GROQ_STT_MODELS,
            Self::OpenAi => super::OPENAI_STT_MODELS,
            // No preset list — the model id is whatever the self-hosted server
            // expects, so the UI shows a free-text field instead of a combo.
            Self::Custom => &[],
        }
    }

    pub(super) fn key_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_KEYS_URL,
            Self::OpenAi => OPENAI_KEYS_URL,
            Self::Custom => "",
        }
    }

    pub(super) fn credential_user(self) -> &'static str {
        match self {
            Self::Groq => "stt-api-key:groq",
            Self::OpenAi => "stt-api-key:openai",
            Self::Custom => "stt-api-key:custom",
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

    pub(super) fn credential_user(self) -> &'static str {
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
        // Self-hosted servers usually need no key; honour an explicit one if set.
        CloudProvider::Custom => &["VOICEPI_STT_API_KEY"],
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

pub(super) fn save_stt_api_key(provider: CloudProvider, secret: &str) -> Result<SecretSaveReport> {
    save_secret(provider.credential_user(), secret)
}

pub(super) fn save_post_api_key(provider: PostProvider, secret: &str) -> Result<SecretSaveReport> {
    save_secret(provider.credential_user(), secret)
}

fn save_secret(user: &str, secret: &str) -> Result<SecretSaveReport> {
    let fallback_path = secret_store_path();
    let credential_target = credential_target_name(user);
    if os_keyring_disabled() {
        save_file_secret(user, secret)?;
        return Ok(SecretSaveReport {
            location: SecretSaveLocation::File,
            credential_service: CREDENTIAL_SERVICE,
            credential_user: user.to_owned(),
            credential_target,
            fallback_path,
            fallback_reason: format!("{DISABLE_OS_KEYRING_ENV} disables OS credential storage"),
        });
    }

    let entry = match credential_entry(user) {
        Ok(entry) => entry,
        Err(err) => {
            let fallback_reason = format!("OS credential store could not be opened: {err}");
            save_file_secret(user, secret).with_context(|| {
                format!(
                    "OS credential store could not be opened ({err}); file fallback also failed"
                )
            })?;
            return Ok(SecretSaveReport {
                location: SecretSaveLocation::File,
                credential_service: CREDENTIAL_SERVICE,
                credential_user: user.to_owned(),
                credential_target,
                fallback_path,
                fallback_reason,
            });
        }
    };

    let keyring_result = {
        let entry = &entry;
        match secret.trim().is_empty() {
            true => match entry.delete_credential() {
                Ok(()) | Err(Error::NoEntry) => Ok(()),
                Err(err) => Err(err.into()),
            },
            false => entry
                .set_password(secret.trim())
                .map_err(anyhow::Error::from),
        }
    };

    match keyring_result {
        Ok(()) => {
            if secret.trim().is_empty() {
                save_file_secret(user, secret)?;
                return Ok(SecretSaveReport {
                    location: SecretSaveLocation::CredentialStore,
                    credential_service: CREDENTIAL_SERVICE,
                    credential_user: user.to_owned(),
                    credential_target,
                    fallback_path,
                    fallback_reason:
                        "cleared OS credential store entry and removed fallback key file entry"
                            .to_owned(),
                });
            }

            match verify_keyring_readback(&entry, secret.trim()) {
                Ok(())
                    if platform_secret_policy()
                        == SecretStorePolicy::WindowsCredentialManagerFirst =>
                {
                    save_file_secret(user, "")?;
                    Ok(SecretSaveReport {
                        location: SecretSaveLocation::CredentialStore,
                        credential_service: CREDENTIAL_SERVICE,
                        credential_user: user.to_owned(),
                        credential_target,
                        fallback_path,
                        fallback_reason:
                            "Windows Credential Manager read-back verified; fallback file entry removed"
                                .to_owned(),
                    })
                }
                Ok(()) => {
                    save_file_fallback_after_keyring_success(user, secret)?;
                    Ok(SecretSaveReport {
                        location: SecretSaveLocation::CredentialStoreAndFile,
                        credential_service: CREDENTIAL_SERVICE,
                        credential_user: user.to_owned(),
                        credential_target,
                        fallback_path,
                        fallback_reason:
                            "kept intentionally so startup still works if the OS credential store cannot read back the key"
                                .to_owned(),
                    })
                }
                Err(readback_err) => {
                    let fallback_reason = format!(
                        "OS credential store write reported success, but read-back verification failed: {readback_err}"
                    );
                    save_file_secret(user, secret).with_context(|| {
                        format!(
                            "OS credential store read-back failed ({readback_err}); file fallback also failed"
                        )
                    })?;
                    Ok(SecretSaveReport {
                        location: SecretSaveLocation::File,
                        credential_service: CREDENTIAL_SERVICE,
                        credential_user: user.to_owned(),
                        credential_target,
                        fallback_path,
                        fallback_reason,
                    })
                }
            }
        }
        Err(keyring_err) => {
            let fallback_reason = format!("OS credential store failed: {keyring_err}");
            save_file_secret(user, secret).with_context(|| {
                format!("OS credential store failed ({keyring_err}); file fallback also failed")
            })?;
            Ok(SecretSaveReport {
                location: SecretSaveLocation::File,
                credential_service: CREDENTIAL_SERVICE,
                credential_user: user.to_owned(),
                credential_target,
                fallback_path,
                fallback_reason,
            })
        }
    }
}

fn verify_keyring_readback(entry: &Entry, expected: &str) -> Result<()> {
    let actual = entry.get_password()?;
    if actual == expected {
        Ok(())
    } else {
        anyhow::bail!("read-back value did not match the saved key")
    }
}

fn credential_target_name(user: &str) -> String {
    if cfg!(windows) {
        format!("{user}.{CREDENTIAL_SERVICE}")
    } else {
        format!("service={CREDENTIAL_SERVICE}; user={user}")
    }
}

fn load_stt_api_key(provider: CloudProvider) -> Result<String> {
    load_secret(provider.credential_user())
}

fn load_post_api_key(provider: PostProvider) -> Result<String> {
    load_secret(provider.credential_user())
}

fn load_secret(user: &str) -> Result<String> {
    if os_keyring_disabled() {
        return load_file_secret(user);
    }

    match credential_entry(user) {
        Ok(entry) => match entry.get_password() {
            Ok(secret) => Ok(secret),
            Err(Error::NoEntry) => load_file_secret(user),
            Err(err) => load_file_secret(user).with_context(|| {
                format!("OS credential store failed ({err}); file fallback failed")
            }),
        },
        Err(err) => load_file_secret(user)
            .with_context(|| format!("OS credential store failed ({err}); file fallback failed")),
    }
}

fn credential_entry(user: &str) -> Result<Entry> {
    ensure_keyring_store()?;
    Entry::new(CREDENTIAL_SERVICE, user).map_err(anyhow::Error::from)
}

fn ensure_keyring_store() -> Result<()> {
    static KEYRING_STORE_INIT: OnceLock<()> = OnceLock::new();
    if KEYRING_STORE_INIT.get().is_none() {
        configure_keyring_store()?;
        let _ = KEYRING_STORE_INIT.set(());
    }
    Ok(())
}

fn configure_keyring_store() -> std::result::Result<(), keyring_core::Error> {
    #[cfg(target_os = "linux")]
    {
        keyring::use_named_store("secret-service")
    }

    #[cfg(not(target_os = "linux"))]
    {
        keyring::use_native_store(false)
    }
}
