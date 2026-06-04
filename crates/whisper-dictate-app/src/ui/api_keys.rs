use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

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
pub(super) const SECRET_STORE_ENV: &str = "VOICEPI_API_KEY_STORE";
pub(super) const DISABLE_OS_KEYRING_ENV: &str = "VOICEPI_DISABLE_OS_KEYRING";
const SECRET_STORE_FILENAME: &str = "api-keys.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SecretSaveLocation {
    CredentialStore,
    File,
}

impl SecretSaveLocation {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::CredentialStore => "OS credential store",
            Self::File => "local fallback key file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SecretSaveReport {
    pub(super) location: SecretSaveLocation,
    pub(super) credential_service: &'static str,
    pub(super) credential_user: String,
    pub(super) fallback_path: PathBuf,
    pub(super) fallback_reason: String,
}

impl SecretSaveReport {
    pub(super) fn status_label(&self) -> String {
        match self.location {
            SecretSaveLocation::CredentialStore => {
                format!(
                    "OS credential store + fallback file: {}",
                    self.fallback_path.display()
                )
            }
            SecretSaveLocation::File => format!(
                "local fallback key file: {} ({})",
                self.fallback_path.display(),
                self.fallback_reason
            ),
        }
    }

    pub(super) fn log_details(&self) -> String {
        format!(
            "location={}; credential_service={}; credential_user={}; fallback_file={}; reason={}",
            self.location.label(),
            self.credential_service,
            self.credential_user,
            self.fallback_path.display(),
            self.fallback_reason
        )
    }
}

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

    pub(super) fn credential_user(self) -> &'static str {
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
    if os_keyring_disabled() {
        save_file_secret(user, secret)?;
        return Ok(SecretSaveReport {
            location: SecretSaveLocation::File,
            credential_service: CREDENTIAL_SERVICE,
            credential_user: user.to_owned(),
            fallback_path,
            fallback_reason: format!("{DISABLE_OS_KEYRING_ENV} disables OS credential storage"),
        });
    }

    let keyring_result = match keyring::Entry::new(CREDENTIAL_SERVICE, user) {
        Ok(entry) => {
            if secret.trim().is_empty() {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(err) => Err(err.into()),
                }
            } else {
                entry
                    .set_password(secret.trim())
                    .map_err(anyhow::Error::from)
            }
        }
        Err(err) => Err(err.into()),
    };
    match keyring_result {
        Ok(()) => {
            save_file_fallback_after_keyring_success(user, secret)?;
            Ok(SecretSaveReport {
                location: SecretSaveLocation::CredentialStore,
                credential_service: CREDENTIAL_SERVICE,
                credential_user: user.to_owned(),
                fallback_path,
                fallback_reason:
                    "kept intentionally so startup still works if the OS credential store cannot read back the key"
                        .to_owned(),
            })
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
                fallback_path,
                fallback_reason,
            })
        }
    }
}

pub(super) fn save_file_fallback_after_keyring_success(user: &str, secret: &str) -> Result<()> {
    save_file_secret(user, secret)
}

pub(super) fn save_file_secret(user: &str, secret: &str) -> Result<()> {
    let mut store = load_secret_store_file()?;
    if secret.trim().is_empty() {
        store.remove(user);
    } else {
        store.insert(user.to_owned(), secret.trim().to_owned());
    }
    write_secret_store_file(&store)
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

    match keyring::Entry::new(CREDENTIAL_SERVICE, user) {
        Ok(entry) => match entry.get_password() {
            Ok(secret) => Ok(secret),
            Err(keyring::Error::NoEntry) => load_file_secret(user),
            Err(err) => load_file_secret(user).with_context(|| {
                format!("OS credential store failed ({err}); file fallback failed")
            }),
        },
        Err(err) => load_file_secret(user)
            .with_context(|| format!("OS credential store failed ({err}); file fallback failed")),
    }
}

fn os_keyring_disabled() -> bool {
    env::var(DISABLE_OS_KEYRING_ENV)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(super) fn load_file_secret(user: &str) -> Result<String> {
    Ok(load_secret_store_file()?.remove(user).unwrap_or_default())
}

fn load_secret_store_file() -> Result<std::collections::BTreeMap<String, String>> {
    let path = secret_store_path();
    if !path.exists() {
        return Ok(Default::default());
    }
    let raw = fs::read_to_string(&path)?;
    if raw.trim().is_empty() {
        return Ok(Default::default());
    }
    serde_json::from_str(&raw).with_context(|| format!("invalid API key store {}", path.display()))
}

fn write_secret_store_file(store: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let path = secret_store_path();
    if store.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_secret_store_contents(&path, &(serde_json::to_string_pretty(store)? + "\n"))
}

#[cfg(unix)]
fn write_secret_store_contents(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_store_contents(path: &std::path::Path, contents: &str) -> Result<()> {
    fs::write(path, contents)?;
    Ok(())
}

fn secret_store_path() -> PathBuf {
    if let Some(raw) = env::var_os(SECRET_STORE_ENV) {
        return PathBuf::from(raw);
    }
    default_secret_store_path()
}

fn default_secret_store_path() -> PathBuf {
    platform_config_dir().join(SECRET_STORE_FILENAME)
}

fn platform_config_dir() -> PathBuf {
    if cfg!(windows) {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("AppData/Roaming"))
            })
            .unwrap_or_else(|| PathBuf::from("."))
            .join("WhisperDictate")
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("whisper-dictate")
    }
}
