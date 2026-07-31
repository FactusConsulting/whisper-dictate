//! Rust-native headless setup and effective-config export.

mod format;
mod wizard;

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Context, Result};

const SECRET_ENVS: [&str; 4] = [
    "VOICEPI_STT_API_KEY",
    "VOICEPI_POST_API_KEY",
    "GROQ_API_KEY",
    "OPENAI_API_KEY",
];

fn resolve_secrets() -> BTreeMap<String, String> {
    resolve_secrets_with(
        &crate::config::load_settings().unwrap_or_default(),
        |name| std::env::var(name).ok(),
        crate::credentials::resolve_stt_api_key,
        crate::credentials::resolve_post_api_key,
    )
}

fn resolve_secrets_with(
    settings: &crate::config::AppSettings,
    env_lookup: impl Fn(&str) -> Option<String>,
    resolve_stt: impl Fn(&str) -> Option<String>,
    resolve_post: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut secrets = SECRET_ENVS
        .iter()
        .filter_map(|name| {
            env_lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(|value| ((*name).to_owned(), value))
        })
        .collect::<BTreeMap<_, _>>();
    if settings.stt_backend == "openai"
        && !secrets.contains_key("VOICEPI_STT_API_KEY")
        && !secrets.contains_key("GROQ_API_KEY")
        && !secrets.contains_key("OPENAI_API_KEY")
    {
        if let Some(secret) = resolve_stt(&settings.stt_base_url) {
            secrets.insert("VOICEPI_STT_API_KEY".to_owned(), secret);
        }
    }
    if matches!(settings.post_processor.as_str(), "openai" | "groq")
        && !secrets.contains_key("VOICEPI_POST_API_KEY")
    {
        if let Some(secret) = resolve_post(&settings.post_base_url) {
            secrets.insert("VOICEPI_POST_API_KEY".to_owned(), secret);
        }
    }
    secrets
}

fn save_minimal_config(config: &BTreeMap<String, String>) -> Result<std::path::PathBuf> {
    let text = format::config_json(config)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    crate::config::AppSettings::from_value(value).context("validate setup values")?;
    let path = crate::config::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

pub fn handle_setup() -> Result<()> {
    let existing = crate::config::effective_runtime_config();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let selected = wizard::run(&existing, &mut stdin.lock(), &mut stdout.lock())?;
    let path = save_minimal_config(&selected)?;
    let mut output = stdout.lock();
    writeln!(output, "\nWrote config to: {}", path.display())?;
    writeln!(
        output,
        "API keys remain in the credential store or environment and are not written to config.json."
    )?;
    writeln!(output, "\n# === PowerShell ===")?;
    writeln!(
        output,
        "{}",
        format::powershell_lines(&selected, &BTreeMap::new())
    )?;
    writeln!(output, "\n# === bash ===")?;
    writeln!(
        output,
        "{}",
        format::bash_lines(&selected, &BTreeMap::new())
    )?;
    Ok(())
}

pub fn handle_export(include_secrets: bool) -> Result<()> {
    let config = crate::config::effective_runtime_config();
    print!(
        "{}",
        format::export_text(&config, &resolve_secrets(), include_secrets)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::{restore_env, ENV_LOCK};

    #[test]
    fn setup_writer_creates_a_valid_minimal_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.json");
        let old = std::env::var_os("VOICEPI_CONFIG");
        std::env::set_var("VOICEPI_CONFIG", &path);

        let values = BTreeMap::from([
            ("model".to_owned(), "small".to_owned()),
            ("max_chars_per_second".to_owned(), "25".to_owned()),
        ]);
        let written = save_minimal_config(&values).unwrap();
        assert_eq!(written, path);
        let settings = crate::config::load_settings_from_path(&path).unwrap();
        assert_eq!(settings.model, "small");
        assert_eq!(settings.max_chars_per_second, "25");

        restore_env("VOICEPI_CONFIG", old);
    }

    #[test]
    fn cloud_secret_fallbacks_use_the_matching_resolvers() {
        let settings = crate::config::AppSettings {
            stt_backend: "openai".to_owned(),
            stt_base_url: "https://stt.example/v1".to_owned(),
            post_processor: "groq".to_owned(),
            post_base_url: "https://post.example/v1".to_owned(),
            ..crate::config::AppSettings::default()
        };
        let secrets = resolve_secrets_with(
            &settings,
            |_| None,
            |url| {
                assert_eq!(url, "https://stt.example/v1");
                Some("stored-stt".to_owned())
            },
            |url| {
                assert_eq!(url, "https://post.example/v1");
                Some("stored-post".to_owned())
            },
        );
        assert_eq!(
            secrets.get("VOICEPI_STT_API_KEY").map(String::as_str),
            Some("stored-stt")
        );
        assert_eq!(
            secrets.get("VOICEPI_POST_API_KEY").map(String::as_str),
            Some("stored-post")
        );
    }

    #[test]
    fn explicit_environment_secrets_win_over_store_fallbacks() {
        let settings = crate::config::AppSettings {
            stt_backend: "openai".to_owned(),
            post_processor: "openai".to_owned(),
            ..crate::config::AppSettings::default()
        };
        let secrets = resolve_secrets_with(
            &settings,
            |name| match name {
                "GROQ_API_KEY" => Some("env-stt".to_owned()),
                "VOICEPI_POST_API_KEY" => Some("env-post".to_owned()),
                _ => None,
            },
            |_| panic!("STT store must not run when an env key exists"),
            |_| panic!("post store must not run when an env key exists"),
        );
        assert_eq!(
            secrets.get("GROQ_API_KEY").map(String::as_str),
            Some("env-stt")
        );
        assert_eq!(
            secrets.get("VOICEPI_POST_API_KEY").map(String::as_str),
            Some("env-post")
        );
    }
}
