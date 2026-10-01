use super::write_secret_store_contents;

#[test]
fn clearing_the_last_file_credential_removes_the_store() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    let _environment =
        crate::ui::test_support::EnvVarGuard::set(super::SECRET_STORE_ENV, path.to_str().unwrap());
    super::save_file_secret("fixture", "synthetic credential").unwrap();
    assert!(path.exists());
    super::save_file_secret("fixture", "").unwrap();
    assert!(!path.exists());
    assert_eq!(super::load_file_secret("fixture").unwrap(), "");
}

#[test]
fn atomic_secret_store_replaces_complete_json_and_keeps_all_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    write_secret_store_contents(&path, r#"{"one":"synthetic-a"}"#).unwrap();
    let next = r#"{"one":"synthetic-a","two":"synthetic-b"}"#;
    write_secret_store_contents(&path, next).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), next);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(windows)]
#[test]
fn locked_secret_store_replacement_keeps_last_good_credentials() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    let previous = r#"{"one":"synthetic-a"}"#;
    write_secret_store_contents(&path, previous).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(write_secret_store_contents(&path, r#"{"one":"synthetic-b"}"#).is_err());
    drop(lock);
    assert_eq!(std::fs::read_to_string(path).unwrap(), previous);
}
