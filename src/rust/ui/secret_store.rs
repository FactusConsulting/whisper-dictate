//! Low-level secret storage primitives shared by `api_keys`: the save-location
//! report types, the OS-keyring policy probe, and the 0600 fallback key-file
//! store with its platform config-dir resolution.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

pub(in crate::ui) const SECRET_STORE_ENV: &str = "VOICEPI_API_KEY_STORE";
pub(in crate::ui) const DISABLE_OS_KEYRING_ENV: &str = "VOICEPI_DISABLE_OS_KEYRING";
const SECRET_STORE_FILENAME: &str = "api-keys.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum SecretSaveLocation {
    CredentialStore,
    CredentialStoreAndFile,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum SecretStorePolicy {
    WindowsCredentialManagerFirst,
    OsKeyringWithFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct SecretSaveReport {
    pub(in crate::ui) location: SecretSaveLocation,
    pub(in crate::ui) credential_service: &'static str,
    pub(in crate::ui) credential_user: String,
    pub(in crate::ui) credential_target: String,
    pub(in crate::ui) fallback_path: PathBuf,
    pub(in crate::ui) fallback_reason: String,
}

impl SecretSaveReport {
    pub(in crate::ui) fn status_label(&self) -> String {
        match self.location {
            SecretSaveLocation::CredentialStore => platform_credential_store_label().to_owned(),
            SecretSaveLocation::CredentialStoreAndFile => format!(
                "{} and fallback key file",
                platform_credential_store_label()
            ),
            SecretSaveLocation::File => format!(
                "fallback key file: {} ({})",
                self.fallback_path.display(),
                self.fallback_reason
            ),
        }
    }

    pub(in crate::ui) fn log_details(&self) -> String {
        match self.location {
            SecretSaveLocation::CredentialStore => format!(
                "stored=credential_store; credential_store={}; credential_service={}; credential_user={}; credential_target={}; verification={}",
                platform_credential_store_label(),
                self.credential_service,
                self.credential_user,
                self.credential_target,
                self.fallback_reason
            ),
            SecretSaveLocation::CredentialStoreAndFile => format!(
                "stored=credential_store_and_fallback_file; credential_store={}; credential_service={}; credential_user={}; credential_target={}; fallback_file={}; reason={}",
                platform_credential_store_label(),
                self.credential_service,
                self.credential_user,
                self.credential_target,
                self.fallback_path.display(),
                self.fallback_reason
            ),
            SecretSaveLocation::File => format!(
                "stored=fallback_file; fallback_file={}; credential_store={}; credential_target={}; reason={}",
                self.fallback_path.display(),
                platform_credential_store_label(),
                self.credential_target,
                self.fallback_reason
            ),
        }
    }
}

pub(in crate::ui) fn platform_secret_policy() -> SecretStorePolicy {
    if cfg!(windows) {
        SecretStorePolicy::WindowsCredentialManagerFirst
    } else {
        SecretStorePolicy::OsKeyringWithFallback
    }
}

pub(in crate::ui) fn platform_credential_store_label() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else {
        "OS credential store"
    }
}

pub(in crate::ui) fn save_file_fallback_after_keyring_success(
    user: &str,
    secret: &str,
) -> Result<()> {
    save_file_secret(user, secret)
}

pub(in crate::ui) fn save_file_secret(user: &str, secret: &str) -> Result<()> {
    let mut store = load_secret_store_file()?;
    if secret.trim().is_empty() {
        store.remove(user);
    } else {
        store.insert(user.to_owned(), secret.trim().to_owned());
    }
    write_secret_store_file(&store)
}

pub(in crate::ui) fn os_keyring_disabled() -> bool {
    env::var(DISABLE_OS_KEYRING_ENV)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(in crate::ui) fn load_file_secret(user: &str) -> Result<String> {
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

pub(in crate::ui) fn secret_store_path() -> PathBuf {
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
