//! On-disk config and filesystem helpers: locating the config file, reading and
//! writing it (preserving unknown keys), managing the dictionary/history files,
//! and opening paths in the platform file manager.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use crate::config::AppSettings;

pub(crate) const CONFIG_ENV: &str = "VOICEPI_CONFIG";

/// Path to the active config.json, honoring the `VOICEPI_CONFIG` override and
/// otherwise falling back to the platform config directory.
pub fn config_path() -> PathBuf {
    if let Some(raw) = env::var_os(CONFIG_ENV) {
        return PathBuf::from(raw);
    }

    platform_config_dir().join("config.json")
}

/// Read the raw config.json as untyped JSON, treating a missing file as `{}`.
pub fn load_raw_config() -> Result<Value> {
    let path = config_path();
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }

    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    Ok(value)
}

/// Load the on-disk config into the typed [`AppSettings`].
pub fn load_settings() -> Result<AppSettings> {
    AppSettings::from_value(load_raw_config()?)
}

/// Persist `settings` to the active config path, preserving unknown keys.
pub fn save_settings(settings: &AppSettings) -> Result<PathBuf> {
    save_settings_to_path(settings, config_path())
}

/// Persist `settings` to `path`, merging into any existing JSON object so that
/// keys not owned by [`AppSettings`] are preserved.
pub fn save_settings_to_path(settings: &AppSettings, path: impl AsRef<Path>) -> Result<PathBuf> {
    settings.validate()?;
    let path = path.as_ref();
    let raw = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let value = if raw.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&raw)?
    };
    let mut object = match value {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    settings.apply_to_object(&mut object);
    path.parent().map(fs::create_dir_all).transpose()?;
    fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(object))? + "\n",
    )?;
    Ok(path.to_path_buf())
}

/// Create an empty JSON dictionary file at `path` if it does not yet exist.
pub fn ensure_dictionary_file(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        path.parent().map(fs::create_dir_all).transpose()?;
        fs::write(path, "{\n  \"terms\": [],\n  \"replacements\": {}\n}\n")?;
    }
    Ok(path.to_path_buf())
}

/// Ensure the dictionary file exists, then open it in the file manager/editor.
pub fn open_dictionary(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = ensure_dictionary_file(path)?;
    open_path(&path)?;
    Ok(path)
}

/// Default location for the dictation history JSONL file.
pub fn default_history_path() -> PathBuf {
    if cfg!(windows) {
        platform_config_dir().join("history.jsonl")
    } else {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .unwrap_or_else(|| PathBuf::from("."))
            .join("whisper-dictate")
            .join("history.jsonl")
    }
}

/// Open an existing path in the file manager, erroring if it is missing.
pub fn open_existing_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(anyhow!("file does not exist: {}", path.display()));
    }
    open_path(path)?;
    Ok(path.to_path_buf())
}

pub(crate) fn platform_config_dir() -> PathBuf {
    if cfg!(windows) {
        let base = env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE")
                    .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        return base.join("WhisperDictate");
    }

    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("whisper-dictate")
}

fn open_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        command
            .args(["/C", "start", "", &path.display().to_string()])
            .creation_flags(0x08000000);
        command.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::ENV_LOCK;

    #[test]
    fn config_env_overrides_default_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.json");

        env::set_var(CONFIG_ENV, &path);
        assert_eq!(config_path(), path);
        env::remove_var(CONFIG_ENV);
    }

    #[test]
    fn missing_config_loads_as_empty_object() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");

        env::set_var(CONFIG_ENV, &path);
        assert_eq!(load_raw_config().unwrap(), Value::Object(Map::new()));
        env::remove_var(CONFIG_ENV);
    }

    #[test]
    fn existing_config_loads_json() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"lang":"da"}"#).unwrap();

        env::set_var(CONFIG_ENV, &path);
        assert_eq!(load_raw_config().unwrap()["lang"], "da");
        env::remove_var(CONFIG_ENV);
    }

    #[test]
    fn saving_settings_preserves_unknown_keys_and_removes_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"unknown":"keep","lang":"da","stt_model":"old","profiles":[{"name":"old"}]}"#,
        )
        .unwrap();

        let settings = AppSettings {
            lang: "en".to_owned(),
            xkb_layout: "dk".to_owned(),
            stt_provider: "groq".to_owned(),
            stt_model: String::new(),
            quit_key: "f12".to_owned(),
            audio_ducking: true,
            post_redact: true,
            post_redact_terms: "Lars Andersen".to_owned(),
            ui_theme: "light".to_owned(),
            ui_language: "da".to_owned(),
            ui_log_view: "debug".to_owned(),
            ui_text_scale: "1.3".to_owned(),
            profiles_json: r#"[{"name":"new"}]"#.to_owned(),
            ..AppSettings::default()
        };

        save_settings_to_path(&settings, &path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert_eq!(saved["unknown"], "keep");
        assert_eq!(saved["lang"], "en");
        assert_eq!(saved["xkb_layout"], "dk");
        assert_eq!(saved["stt_provider"], "groq");
        assert_eq!(saved["quit_key"], "f12");
        assert_eq!(saved["audio_ducking"], "1");
        assert_eq!(saved["post_redact"], "1");
        assert_eq!(saved["post_redact_terms"], "Lars Andersen");
        assert_eq!(saved["ui_theme"], "light");
        assert_eq!(saved["ui_language"], "da");
        assert_eq!(saved["ui_log_view"], "debug");
        assert_eq!(saved["ui_text_scale"], "1.3");
        assert!(saved.get("stt_model").is_none());
        assert_eq!(saved["profiles"][0]["name"], "new");
    }

    #[test]
    fn saving_empty_profiles_removes_profiles_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"profiles":[{"name":"old"}]}"#).unwrap();

        save_settings_to_path(&AppSettings::default(), &path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert!(saved.get("profiles").is_none());
    }

    #[test]
    fn ensure_dictionary_file_creates_empty_json_dictionary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");

        ensure_dictionary_file(&path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert_eq!(saved["terms"], serde_json::json!([]));
        assert_eq!(saved["replacements"], serde_json::json!({}));
    }
}
