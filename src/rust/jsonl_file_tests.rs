use super::*;

#[test]
fn regular_file_published_after_missing_probe_keeps_the_same_lock_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let expected = missing_identity(&path).unwrap();
    fs::write(&path, b"published row\n").unwrap();
    // Exercise a stale NotFound result from canonicalize deterministically.
    assert_eq!(missing_identity(&path).unwrap(), expected);
    assert_eq!(identity(&path).unwrap(), expected);
    assert_eq!(fs::read(&path).unwrap(), b"published row\n");
}

#[cfg(unix)]
#[test]
fn missing_identity_still_rejects_a_dangling_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let alias = dir.path().join("alias.jsonl");
    std::os::unix::fs::symlink(dir.path().join("missing.jsonl"), &alias).unwrap();
    assert!(missing_identity(&alias)
        .unwrap_err()
        .to_string()
        .contains("dangling alias"));
    assert!(identity(&alias).is_err());
    assert!(!sibling(&alias, ".wd-write.lock").unwrap().exists());
}

#[cfg(windows)]
#[test]
fn windows_identity_comparison_preserves_unicode_without_ascii_folding() {
    assert!(same_identity(Path::new("rødgrød.jsonl"), Path::new("RØDGRØD.JSONL")).unwrap());
    assert!(!same_identity(Path::new("rødgrød.jsonl"), Path::new("RÅDGRØD.JSONL")).unwrap());
    assert!(same_identity(Path::new(""), Path::new("")).unwrap());
    assert!(!same_identity(Path::new(""), Path::new("rødgrød.jsonl")).unwrap());
}

#[test]
fn competing_writer_times_out_without_changing_data_and_can_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"last good").unwrap();
    let held = acquire(&path).unwrap();
    assert_eq!(
        acquire_for(&path, Duration::from_millis(15))
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(fs::read(&path).unwrap(), b"last good");
    drop(held);
    let next = acquire(&path).unwrap();
    assert_eq!(next.path, fs::canonicalize(&path).unwrap());
    assert!(sibling(&path, ".wd-write.lock").unwrap().exists());
}

#[test]
fn general_jsonl_writers_cooperate_with_retention_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let held = acquire(&path).unwrap();
    let alternate = dir.path().join(".").join("history.jsonl");
    assert_eq!(
        acquire_for(&alternate, Duration::from_millis(15))
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
    drop(held);
    crate::telemetry::append_jsonl(&path, &serde_json::json!({"text":"cooperating"})).unwrap();
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "{\"text\":\"cooperating\"}\n"
    );
}

#[cfg(unix)]
#[test]
fn provisioned_lock_allows_append_without_directory_write_permission() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"{\"text\":\"keep\"}\n").unwrap();
    fs::write(sibling(&path, ".wd-write.lock").unwrap(), b"").unwrap();
    let original_permissions = fs::metadata(dir.path()).unwrap().permissions();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = crate::telemetry::append_jsonl(&path, &serde_json::json!({"text":"new"}));
    fs::set_permissions(dir.path(), original_permissions).unwrap();
    result.unwrap();
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "{\"text\":\"keep\"}\n{\"text\":\"new\"}\n"
    );
}

#[cfg(unix)]
#[test]
fn symlink_alias_uses_same_writer_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"last good").unwrap();
    let alias = dir.path().join("alias.jsonl");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let held = acquire(&path).unwrap();
    assert_eq!(
        acquire_for(&alias, Duration::from_millis(15))
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
    drop(held);
}
