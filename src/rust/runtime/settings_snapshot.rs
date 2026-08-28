//! Immutable native-runtime configuration and scoped credentials.
//!
//! Environment variables remain supported at process boundaries, but the
//! in-process runtime consumes this owned snapshot after resolution. Secret
//! values are deliberately kept outside [`crate::config::AppSettings`] and
//! omitted from `Debug` output.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::config::AppSettings;

pub(crate) const STT_API_KEY_ENV: &str = "VOICEPI_STT_API_KEY";
pub(crate) const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY";
pub(crate) const GROQ_API_KEY_ENV: &str = "GROQ_API_KEY";
pub(crate) const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub(crate) const POST_API_KEY_ENDPOINT_ENV: &str = "VOICEPI_POST_API_KEY_ENDPOINT";

pub(crate) const CREDENTIAL_ENV_NAMES: [&str; 5] = [
    STT_API_KEY_ENV,
    POST_API_KEY_ENV,
    GROQ_API_KEY_ENV,
    OPENAI_API_KEY_ENV,
    POST_API_KEY_ENDPOINT_ENV,
];

const COMPAT_BOUNDARY_ENV_NAMES: [&str; 3] = [
    "VOICEPI_WHISPER_MODEL_PATH",
    crate::whisper::IDLE_UNLOAD_ENV,
    crate::whisper::GPU_ENV,
];

#[derive(Clone, Default, PartialEq, Eq)]
struct ScopedCredentials {
    stt: Option<String>,
    post: Option<String>,
    groq: Option<String>,
    openai: Option<String>,
    post_endpoint: Option<String>,
}

impl ScopedCredentials {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            STT_API_KEY_ENV => self.stt.as_deref(),
            POST_API_KEY_ENV => self.post.as_deref(),
            GROQ_API_KEY_ENV => self.groq.as_deref(),
            OPENAI_API_KEY_ENV => self.openai.as_deref(),
            POST_API_KEY_ENDPOINT_ENV => self.post_endpoint.as_deref(),
            _ => None,
        }
    }

    fn set(&mut self, name: &str, value: String) -> bool {
        let slot = match name {
            STT_API_KEY_ENV => &mut self.stt,
            POST_API_KEY_ENV => &mut self.post,
            GROQ_API_KEY_ENV => &mut self.groq,
            OPENAI_API_KEY_ENV => &mut self.openai,
            POST_API_KEY_ENDPOINT_ENV => &mut self.post_endpoint,
            _ => return false,
        };
        *slot = (!value.trim().is_empty()).then_some(value);
        true
    }

    fn present_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        CREDENTIAL_ENV_NAMES
            .into_iter()
            .filter(|name| self.get(name).is_some())
    }
}

/// Typed settings plus provider-scoped credentials for one native session.
#[derive(Clone, PartialEq)]
pub(crate) struct RuntimeSettingsSnapshot {
    settings: AppSettings,
    /// Effective STT endpoint before any per-invocation overrides are applied.
    ///
    /// Credential provenance checks need to distinguish an endpoint persisted
    /// in the selected config from a later `VOICEPI_STT_BASE_URL` override.
    /// Keeping this small immutable baseline in the session snapshot prevents
    /// `set()` from erasing that distinction before saved-key resolution.
    initial_stt_base_url: String,
    stt_provider: String,
    /// `AppSettings` supplies `openai` as the schema default, but that value
    /// must not mask a provider inferred from environment-only Nemotron
    /// configuration. Keep explicit ownership separate from the typed value.
    stt_provider_explicit: bool,
    values: BTreeMap<String, String>,
    credentials: ScopedCredentials,
    ambient_credentials: BTreeSet<String>,
}

impl std::fmt::Debug for RuntimeSettingsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSettingsSnapshot")
            .field("settings", &self.settings)
            .field("value_names", &self.values.keys().collect::<Vec<_>>())
            .field(
                "credential_names",
                &self.credentials.present_names().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Default for RuntimeSettingsSnapshot {
    fn default() -> Self {
        Self::from_pairs(Vec::<(String, String)>::new()).expect("default runtime settings snapshot")
    }
}

impl RuntimeSettingsSnapshot {
    pub(crate) fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        Self::from_pairs_with_ambient(pairs, |_| None)
    }

    /// Resolve documented compatibility variables exactly once at the process
    /// boundary. Callers can inject the lookup for hermetic precedence tests.
    pub(crate) fn from_pairs_with_ambient(
        pairs: impl IntoIterator<Item = (String, String)>,
        ambient: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let mut values = BTreeMap::new();
        let mut credentials = ScopedCredentials::default();
        let mut ambient_credentials = BTreeSet::new();
        for (name, value) in pairs {
            if !credentials.set(&name, value.clone()) {
                values.insert(name, value);
            }
        }
        for name in CREDENTIAL_ENV_NAMES {
            if credentials.get(name).is_none() {
                if let Some(value) = ambient(name).filter(|value| !value.trim().is_empty()) {
                    credentials.set(name, value);
                    ambient_credentials.insert(name.to_owned());
                }
            }
        }
        for name in COMPAT_BOUNDARY_ENV_NAMES {
            if !values.contains_key(name) {
                if let Some(value) = ambient(name).filter(|value| !value.trim().is_empty()) {
                    values.insert(name.to_owned(), value);
                }
            }
        }
        let settings = typed_settings(&values)?;
        Ok(Self {
            initial_stt_base_url: settings.stt_base_url.clone(),
            settings,
            stt_provider: "openai".to_owned(),
            stt_provider_explicit: false,
            values,
            credentials,
            ambient_credentials,
        })
    }

    pub(crate) fn settings(&self) -> &AppSettings {
        &self.settings
    }

    /// Endpoint owned by the snapshot at construction time, before mutable
    /// CLI/live overrides changed [`Self::settings`].
    #[allow(dead_code)]
    pub(crate) fn initial_stt_base_url(&self) -> &str {
        &self.initial_stt_base_url
    }

    #[allow(dead_code)]
    pub(crate) fn stt_provider(&self) -> &str {
        &self.stt_provider
    }

    #[allow(dead_code)]
    pub(crate) fn has_explicit_stt_provider(&self) -> bool {
        self.stt_provider_explicit
    }

    #[allow(dead_code)]
    pub(crate) fn set_stt_provider(&mut self, provider: impl Into<String>) {
        let provider = provider.into();
        self.stt_provider = provider.clone();
        self.settings.stt_provider = provider;
        self.stt_provider_explicit = true;
        if let Some(raw_device) = self.values.get("VOICEPI_DEVICE") {
            let provider_for_device = if self.settings.stt_backend.eq_ignore_ascii_case("openai") {
                self.stt_provider.as_str()
            } else {
                ""
            };
            self.settings.device =
                crate::whisper::device_options::canonicalize_device_value_for_provider(
                    raw_device,
                    provider_for_device,
                );
        }
        if crate::cloud_api::is_nemotron_provider(&self.stt_provider) {
            // `stt_provider` is UI-owned rather than a schema-backed worker
            // variable, so the raw `VOICEPI_STT_BASE_URL` pair may still hold
            // an older NIM HTTP value when the provider is attached here.
            // Migrate both the typed settings and the owned pair before the
            // in-process backend reads the snapshot. Credential resolution
            // and the gRPC transcriber consume `value()`, not only
            // `settings()`, so the two representations must stay in sync.
            let defaults = AppSettings::default();
            let configured_base = self.settings.stt_base_url.clone();
            let migrated = crate::cloud_api::migrate_nemotron_endpoint(
                &configured_base,
                &defaults.stt_base_url,
            );
            let initial_matches_pair = self
                .values
                .get(crate::dictate::backends::cloud_transcribe::STT_BASE_URL_ENV)
                .map(|value| value.trim() == self.initial_stt_base_url.trim())
                .unwrap_or(true);
            self.settings.stt_base_url = migrated.clone();
            self.values.insert(
                crate::dictate::backends::cloud_transcribe::STT_BASE_URL_ENV.to_owned(),
                migrated.clone(),
            );
            if initial_matches_pair {
                self.initial_stt_base_url = migrated;
            }
        }
    }

    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.credentials
            .get(name)
            .or_else(|| self.values.get(name).map(String::as_str))
    }

    pub(crate) fn set(&mut self, name: impl Into<String>, value: impl Into<String>) -> Result<()> {
        let name = name.into();
        let value = value.into();
        if self.credentials.set(&name, value.clone()) {
            self.ambient_credentials.remove(&name);
            return Ok(());
        }
        let is_device = name == "VOICEPI_DEVICE";
        let device_value = value.clone();
        self.values.insert(name, value);
        self.settings = typed_settings(&self.values)?;
        if is_device {
            let provider_for_device = if self.settings.stt_backend.eq_ignore_ascii_case("openai") {
                self.stt_provider.as_str()
            } else {
                ""
            };
            self.settings.device =
                crate::whisper::device_options::canonicalize_device_value_for_provider(
                    &device_value,
                    provider_for_device,
                );
        }
        Ok(())
    }

    pub(crate) fn credential_is_ambient(&self, name: &str, value: &str) -> bool {
        self.ambient_credentials.contains(name) && self.value(name) == Some(value)
    }

    pub(crate) fn value_count(&self) -> usize {
        self.values.len() + self.credentials.present_names().count()
    }

    pub(crate) fn value_names(&self) -> Vec<String> {
        let mut names = self.values.keys().cloned().collect::<Vec<_>>();
        names.extend(self.credentials.present_names().map(str::to_owned));
        names.sort_unstable();
        names
    }

    #[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
    pub(crate) fn pairs_owned(&self) -> Vec<(String, String)> {
        let mut pairs = self
            .values
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        pairs.extend(self.credentials.present_names().filter_map(|name| {
            self.credentials
                .get(name)
                .map(|value| (name.to_owned(), value.to_owned()))
        }));
        pairs
    }
}

fn typed_settings(values: &BTreeMap<String, String>) -> Result<AppSettings> {
    let mut object = Map::new();
    for setting in crate::config::runtime_settings() {
        if let Some(value) = values.get(&setting.env) {
            object.insert(setting.key.clone(), Value::String(value.clone()));
        }
    }
    AppSettings::from_value(Value::Object(object)).context("parse native runtime settings snapshot")
}

/// Prevent credentials owned by the native session from reaching helper
/// processes. Environment-provided keys are scrubbed too: a child command is
/// not a provider request and has no reason to receive them.
pub(crate) fn scrub_credentials_from_child(command: &mut std::process::Command) {
    for name in CREDENTIAL_ENV_NAMES {
        command.env_remove(name);
    }
}

#[cfg(test)]
#[path = "settings_snapshot_tests.rs"]
mod tests;
