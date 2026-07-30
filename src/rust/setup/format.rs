//! Pure config and shell formatters for headless setup/export.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{Map, Value};

pub(super) const REDACTED: &str = "***";

pub fn config_json(config: &BTreeMap<String, String>) -> Result<String> {
    let mut object = Map::new();
    for setting in crate::config::runtime_settings() {
        if let Some(value) = config.get(&setting.key) {
            object.insert(setting.key.clone(), Value::String(value.clone()));
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&object)?))
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn bash_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._/:".contains(c))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn env_lines(
    config: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    formatter: impl Fn(&str, &str) -> String,
) -> String {
    let mut lines = Vec::new();
    for setting in crate::config::runtime_settings() {
        if let Some(value) = config.get(&setting.key) {
            lines.push(formatter(&setting.env, value));
        }
    }
    for (name, value) in secrets {
        lines.push(formatter(name, value));
    }
    lines.join("\n")
}

pub fn powershell_lines(
    config: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> String {
    env_lines(config, secrets, |name, value| {
        format!("$env:{name} = {}", powershell_quote(value))
    })
}

pub fn bash_lines(config: &BTreeMap<String, String>, secrets: &BTreeMap<String, String>) -> String {
    env_lines(config, secrets, |name, value| {
        format!("export {name}={}", bash_quote(value))
    })
}

pub fn export_text(
    config: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    include_secrets: bool,
) -> Result<String> {
    let shown = secrets
        .keys()
        .map(|name| {
            (
                name.clone(),
                if include_secrets {
                    secrets[name].clone()
                } else {
                    REDACTED.to_owned()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut output = String::from("# === config.json ===\n");
    output.push_str(&config_json(config)?);
    output.push('\n');
    if !shown.is_empty() && !include_secrets {
        output.push_str(
            "# secrets redacted (***) - re-run with --include-secrets to emit them in full\n",
        );
    }
    output.push_str("# === PowerShell ===\n");
    output.push_str(&powershell_lines(config, &shown));
    output.push_str("\n\n# === bash ===\n");
    output.push_str(&bash_lines(config, &shown));
    output.push('\n');
    Ok(output)
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
