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

    #[cfg(test)]
    {
        // Unit tests must NEVER fall back to the developer's real
        // config.json. They did, and it made the suite pass or fail
        // depending on whose machine it ran on: on a box configured for
        // cloud STT, 81 tests failed with things like "openai benchmark
        // backend requires a cloud API key", while the same commit was green
        // in CI -- runners have no user config, so the bug could only ever be
        // seen locally, by the people least likely to suspect the harness.
        //
        // A test that needs config content sets `VOICEPI_CONFIG` itself (most
        // already do); everything else now gets a path that does not exist,
        // which `load_raw_config` treats as `{}` -- i.e. schema defaults.
        // Per-process so a parallel run cannot collide, and deliberately not
        // created: a test that writes here without opting in should fail
        // loudly rather than silently share state with its neighbours.
        std::env::temp_dir()
            .join(format!(
                "whisper-dictate-test-no-config-{}",
                std::process::id()
            ))
            .join("config.json")
    }
    #[cfg(not(test))]
    {
        platform_config_dir().join("config.json")
    }
}

/// Read the raw config.json as untyped JSON, treating a missing file as `{}`.
pub fn load_raw_config() -> Result<Value> {
    load_raw_config_from_path(&config_path())
}

/// Read the raw config JSON at an explicit path, treating a missing file as
/// `{}`. Same shape as [`load_raw_config`] but honours a caller-supplied
/// override (used by the `config get --config PATH` / `config set --config
/// PATH` CLI verbs so a script can point at a scratch file without mutating
/// the process's `VOICEPI_CONFIG` env var).
pub fn load_raw_config_from_path(path: &Path) -> Result<Value> {
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

/// Load an explicit config file into the typed [`AppSettings`]. Missing file
/// yields `AppSettings::default()`. Companion to [`load_raw_config_from_path`]
/// for the CLI `config get`/`list` verbs.
pub fn load_settings_from_path(path: &Path) -> Result<AppSettings> {
    AppSettings::from_value(load_raw_config_from_path(path)?)
}

/// Persist `settings` to the active config path, preserving unknown keys.
pub fn save_settings(settings: &AppSettings) -> Result<PathBuf> {
    save_settings_to_path(settings, config_path())
}

/// Persist `settings` to `path`, merging into any existing JSON object so that
/// keys not owned by [`AppSettings`] are preserved.
pub fn save_settings_to_path(settings: &AppSettings, path: impl AsRef<Path>) -> Result<PathBuf> {
    save_settings_to_path_with_explicit_nulls(settings, path, &[])
}

/// Persist settings while recording which nullable keys a focused mutation
/// explicitly cleared, even when those keys were previously absent on disk.
pub(crate) fn save_settings_to_path_with_explicit_nulls(
    settings: &AppSettings,
    path: impl AsRef<Path>,
    explicit_nulls: &[&str],
) -> Result<PathBuf> {
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
    settings.apply_to_object_with_explicit_nulls(&mut object, explicit_nulls);
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

pub fn platform_config_dir() -> PathBuf {
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
        crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
        command.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(path);
        crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
        command.spawn()?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
        command.spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::ENV_LOCK;

    #[test]
    fn without_the_env_override_tests_never_reach_the_users_real_config() {
        // The regression this guards: `config_path()` used to fall back to
        // `platform_config_dir()/config.json`, so the unit suite read whoever
        // was running it. On a machine configured for cloud STT that failed 81
        // tests, while CI stayed green because runners have no user config --
        // the worst shape for a harness bug, visible only to the people least
        // likely to suspect the harness.
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = env::var_os(CONFIG_ENV);
        env::remove_var(CONFIG_ENV);

        let path = config_path();

        if let Some(previous) = previous {
            env::set_var(CONFIG_ENV, previous);
        }

        assert!(
            !path.exists(),
            "unit tests must start from schema defaults, not a file on disk: {}",
            path.display()
        );
        assert_ne!(
            path,
            platform_config_dir().join("config.json"),
            "config_path() must not fall back to the operator's real config in tests"
        );
        // Per-process, so a parallel `cargo test` cannot collide.
        assert!(
            path.to_string_lossy()
                .contains(&std::process::id().to_string()),
            "the test-only path should be process-scoped: {}",
            path.display()
        );
    }

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
    fn saving_settings_preserves_unknown_keys_and_persists_explicit_clears() {
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
        assert_eq!(saved["audio_ducking"], "1");
        assert_eq!(saved["post_redact"], "1");
        assert_eq!(saved["post_redact_terms"], "Lars Andersen");
        assert_eq!(saved["ui_theme"], "light");
        assert_eq!(saved["ui_language"], "da");
        assert_eq!(saved["ui_log_view"], "debug");
        assert_eq!(saved["ui_text_scale"], "1.3");
        assert_eq!(saved["stt_model"], Value::Null);
        assert_eq!(saved["profiles"][0]["name"], "new");
    }

    #[test]
    fn saving_empty_profiles_removes_profiles_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"profiles":[{"name":"old"}]}"#).unwrap();

        // Empty profiles explicitly (the default now ships an example profile).
        let settings = AppSettings {
            profiles_json: "[]".to_owned(),
            ..AppSettings::default()
        };
        save_settings_to_path(&settings, &path).unwrap();
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
