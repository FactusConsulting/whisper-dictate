use std::collections::BTreeMap;
use std::io::{self, Read};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

const LOCAL_BACKENDS: &[&str] = &["whisper", "faster-whisper", "parakeet"];
const LOCAL_PROCESSORS: &[&str] = &["none", "ollama"];
const OFFLINE_ENV: &[&str] = &[
    "HF_HUB_OFFLINE",
    "TRANSFORMERS_OFFLINE",
    "HF_DATASETS_OFFLINE",
    "HF_HUB_DISABLE_TELEMETRY",
];

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum PrivacyRequest {
    EnvUpdates {
        local_only: bool,
    },
    AssertBackend {
        local_only: bool,
        backend: String,
        #[serde(default = "default_feature")]
        feature: String,
    },
    AssertProcessor {
        local_only: bool,
        processor: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvUpdates {
    pub enabled: bool,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrivacyCheck {
    pub ok: bool,
    pub error: Option<String>,
}

pub fn handle_privacy() -> Result<()> {
    let request = read_request()?;
    match request {
        PrivacyRequest::EnvUpdates { local_only } => {
            println!(
                "{}",
                serde_json::to_string(&local_only_env_updates(local_only))?
            );
        }
        PrivacyRequest::AssertBackend {
            local_only,
            backend,
            feature,
        } => {
            println!(
                "{}",
                serde_json::to_string(&check_result(assert_local_backend(
                    local_only, &backend, &feature
                )))?
            );
        }
        PrivacyRequest::AssertProcessor {
            local_only,
            processor,
        } => {
            println!(
                "{}",
                serde_json::to_string(&check_result(assert_local_processor(
                    local_only, &processor
                )))?
            );
        }
    }
    Ok(())
}

pub fn truthy(value: Option<&str>) -> bool {
    !matches!(
        value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

pub fn local_only_env_updates(local_only: bool) -> EnvUpdates {
    let mut env = BTreeMap::new();
    if local_only {
        for name in OFFLINE_ENV {
            env.insert((*name).to_owned(), "1".to_owned());
        }
        env.insert("WANDB_DISABLED".to_owned(), "true".to_owned());
        env.insert("WANDB_MODE".to_owned(), "offline".to_owned());
    }
    EnvUpdates {
        enabled: local_only,
        env,
    }
}

pub fn assert_local_backend(local_only: bool, backend: &str, feature: &str) -> Result<()> {
    if !local_only {
        return Ok(());
    }
    let normalized = backend.trim().to_ascii_lowercase();
    if LOCAL_BACKENDS.contains(&normalized.as_str()) {
        return Ok(());
    }
    Err(anyhow!(
        "VOICEPI_LOCAL_ONLY=1 blocks {feature} backend {backend:?}; choose a local backend or disable local-only mode."
    ))
}

pub fn assert_local_processor(local_only: bool, processor: &str) -> Result<()> {
    if !local_only {
        return Ok(());
    }
    let normalized = processor.trim().to_ascii_lowercase();
    if LOCAL_PROCESSORS.contains(&normalized.as_str()) {
        return Ok(());
    }
    Err(anyhow!(
        "VOICEPI_LOCAL_ONLY=1 blocks post-processing provider {processor:?}; choose a local provider or disable local-only mode."
    ))
}

fn check_result(result: Result<()>) -> PrivacyCheck {
    match result {
        Ok(()) => PrivacyCheck {
            ok: true,
            error: None,
        },
        Err(err) => PrivacyCheck {
            ok: false,
            error: Some(err.to_string()),
        },
    }
}

fn default_feature() -> String {
    "STT".to_owned()
}

fn read_request() -> Result<PrivacyRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_matches_python_semantics() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
        ] {
            assert!(!truthy(value));
        }
        assert!(truthy(Some("1")));
        assert!(truthy(Some("yes")));
        assert!(truthy(Some("anything")));
    }

    #[test]
    fn local_only_env_updates_include_offline_gates() {
        let updates = local_only_env_updates(true);

        assert!(updates.enabled);
        assert_eq!(updates.env["HF_HUB_OFFLINE"], "1");
        assert_eq!(updates.env["WANDB_DISABLED"], "true");
        assert_eq!(updates.env["WANDB_MODE"], "offline");
    }

    #[test]
    fn local_only_env_updates_are_empty_when_disabled() {
        let updates = local_only_env_updates(false);

        assert!(!updates.enabled);
        assert!(updates.env.is_empty());
    }

    #[test]
    fn local_only_blocks_remote_backend_and_processor() {
        let backend = assert_local_backend(true, "openai:gpt-4o-transcribe", "STT")
            .unwrap_err()
            .to_string();
        let processor = assert_local_processor(true, "openai")
            .unwrap_err()
            .to_string();

        assert!(backend.contains("VOICEPI_LOCAL_ONLY=1 blocks STT backend"));
        assert!(processor.contains("blocks post-processing provider"));
    }

    #[test]
    fn local_only_allows_local_backends_and_processors() {
        for backend in LOCAL_BACKENDS {
            assert_local_backend(true, backend, "STT").unwrap();
        }
        for processor in LOCAL_PROCESSORS {
            assert_local_processor(true, processor).unwrap();
        }
    }
}
