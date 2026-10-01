use super::*;

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
