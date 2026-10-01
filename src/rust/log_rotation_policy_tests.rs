use super::*;
use crate::config::test_support::{restore_env, ENV_LOCK};

struct Fixture {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    old: Vec<(&'static str, Option<std::ffi::OsString>)>,
}
impl Fixture {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let old = [
            "VOICEPI_CONFIG",
            "VOICEPI_HISTORY_JSONL",
            "VOICEPI_METRICS_JSONL",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        std::env::set_var("VOICEPI_CONFIG", dir.path().join("config.json"));
        std::env::remove_var("VOICEPI_HISTORY_JSONL");
        std::env::remove_var("VOICEPI_METRICS_JSONL");
        fs::write(dir.path().join("config.json"), b"{}").unwrap();
        Self {
            _lock: lock,
            dir,
            old,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in self.old.drain(..) {
            restore_env(key, value);
        }
    }
}

#[test]
fn configured_history_metrics_and_recovery_paths_are_protected_even_when_disabled() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("gui.log");
    let backup = jsonl_file::sibling(&path, ".previous").unwrap();
    for key in ["history_jsonl", "metrics_jsonl"] {
        for protected in [&path, &backup] {
            fs::write(&path, b"lastgood\n").unwrap();
            let raw =
                serde_json::json!({key: protected, "history_enabled": false, "json_output": false});
            fs::write(config::config_path(), serde_json::to_vec(&raw).unwrap()).unwrap();
            let mut writer = RotatingLog::open_with(&path, 9, protect_user_data).unwrap();
            writeln!(writer, "next").unwrap();
            assert!(writer.flush().is_err(), "{key}: {protected:?}");
            assert_eq!(fs::read(&path).unwrap(), b"lastgood\n");
            assert!(!backup.exists());
        }
    }
    let history = fixture.dir.path().join("history");
    let recovery = jsonl_file::sibling(&history, ".retention-backup").unwrap();
    fs::write(&recovery, b"user recovery\n").unwrap();
    fs::write(
        config::config_path(),
        serde_json::to_vec(&serde_json::json!({"history_jsonl": history})).unwrap(),
    )
    .unwrap();
    assert!(protect_user_data(&jsonl_file::identity(&recovery).unwrap(), &backup).is_err());
}

#[test]
fn env_overlay_snapshot_and_invalid_config_fail_closed() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    let active = jsonl_file::identity(&path).unwrap();
    let backup = jsonl_file::sibling(&active, ".previous").unwrap();
    std::env::set_var("VOICEPI_METRICS_JSONL", &path);
    assert!(protect_user_data(&active, &backup).is_err());
    // Config takes precedence over a conflicting inherited env override.
    fs::write(
        config::config_path(),
        serde_json::to_vec(
            &serde_json::json!({"metrics_jsonl": fixture.dir.path().join("separate")}),
        )
        .unwrap(),
    )
    .unwrap();
    protect_user_data(&active, &backup).unwrap();
    for malformed in ["{invalid", "[]", "null"] {
        fs::write(config::config_path(), malformed).unwrap();
        assert!(protect_user_data(&active, &backup).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"lastgood\n");
    }
}

#[cfg(windows)]
#[test]
fn windows_case_alias_of_missing_backup_is_protected() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    let active = jsonl_file::identity(&path).unwrap();
    let backup = jsonl_file::sibling(&active, ".previous").unwrap();
    std::env::set_var(
        "VOICEPI_METRICS_JSONL",
        fixture.dir.path().join("GUI.LOG.PREVIOUS"),
    );
    assert!(protect_user_data(&active, &backup).is_err());
    assert!(!backup.exists());
}

#[cfg(unix)]
#[test]
fn configured_symlink_alias_is_protected() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    let alias = fixture.dir.path().join("alias");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    std::env::set_var("VOICEPI_HISTORY_JSONL", alias);
    let active = jsonl_file::identity(&path).unwrap();
    assert!(
        protect_user_data(&active, &jsonl_file::sibling(&active, ".previous").unwrap()).is_err()
    );
}
