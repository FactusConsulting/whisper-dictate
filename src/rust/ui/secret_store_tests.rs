use super::write_secret_store_contents;

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
