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
    let mut secrets = SECRET_ENVS
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(|value| ((*name).to_owned(), value))
        })
        .collect::<BTreeMap<_, _>>();
    let settings = crate::config::load_settings().unwrap_or_default();
    if settings.stt_backend == "openai"
        && !secrets.contains_key("VOICEPI_STT_API_KEY")
        && !secrets.contains_key("GROQ_API_KEY")
        && !secrets.contains_key("OPENAI_API_KEY")
    {
        if let Some(secret) = crate::credentials::resolve_stt_api_key(&settings.stt_base_url) {
            secrets.insert("VOICEPI_STT_API_KEY".to_owned(), secret);
        }
    }
    if matches!(settings.post_processor.as_str(), "openai" | "groq")
        && !secrets.contains_key("VOICEPI_POST_API_KEY")
    {
        if let Some(secret) = crate::credentials::resolve_post_api_key(&settings.post_base_url) {
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
            ("beam_size".to_owned(), "3".to_owned()),
        ]);
        let written = save_minimal_config(&values).unwrap();
        assert_eq!(written, path);
        let settings = crate::config::load_settings_from_path(&path).unwrap();
        assert_eq!(settings.model, "small");
        assert_eq!(settings.beam_size, "3");

        restore_env("VOICEPI_CONFIG", old);
    }
}
