use super::*;
use serde_json::json;

fn text(n: usize) -> Value { json!({"text": format!("rødgrød-{n}")}) }
fn rows(path: &Path) -> Vec<Value> {
    fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn unlimited_is_default_and_never_prunes_or_creates_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"malformed\n").unwrap();
    append(&path, &text(0), 0, None).unwrap();
    assert!(fs::read(&path).unwrap().starts_with(b"malformed\n"));
    assert!(!jsonl_file::sibling(&path, ".retention-backup").unwrap().exists());
    for invalid in ["", "-1", "NaN", "1.5", "100001", "18446744073709551616"] {
        assert_eq!(parse_limit(invalid), 0);
    }
    assert_eq!(parse_limit("100000"), MAX_ENTRIES);
}

#[test]
fn missing_empty_boundary_unicode_and_one_previous_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let backup = jsonl_file::sibling(&path, ".retention-backup").unwrap();
    append(&path, &text(0), 2, None).unwrap();
    append(&path, &text(1), 2, None).unwrap();
    assert_eq!(rows(&path), vec![text(0), text(1)]);
    assert!(!backup.exists());
    append(&path, &text(2), 2, None).unwrap();
    assert_eq!(rows(&path), vec![text(1), text(2)]);
    assert_eq!(rows(&backup), vec![text(0), text(1)]);
    append(&path, &text(3), 2, None).unwrap();
    assert_eq!(rows(&path), vec![text(2), text(3)]);
    assert_eq!(rows(&backup), vec![text(1), text(2)]);
    fs::write(&path, b"").unwrap();
    append(&path, &text(4), 1, None).unwrap();
    assert_eq!(rows(&path), vec![text(4)]);
    append(&path, &text(5), 1, None).unwrap();
    assert_eq!(rows(&path), vec![text(5)]);
    assert_eq!(rows(&backup), vec![text(4)]);
}

#[test]
fn malformed_partial_nonhistory_or_oversized_rows_preserve_original_and_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let backup = jsonl_file::sibling(&path, ".retention-backup").unwrap();
    fs::write(&backup, b"previous recovery").unwrap();
    for original in [b"not json\n".to_vec(), b"{\"text\":\"partial".to_vec(), b"{\"metrics_only\":true}\n".to_vec(), b"[]\n".to_vec(), vec![b'x'; MAX_LINE_BYTES + 1]] {
        fs::write(&path, &original).unwrap();
        assert!(append(&path, &text(1), 1, None).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(&backup).unwrap(), b"previous recovery");
    }
}

#[test]
fn valid_final_row_without_newline_is_separated_in_append_and_prune() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    for limit in [1, 2] {
        fs::write(&path, serde_json::to_vec(&text(0)).unwrap()).unwrap();
        append(&path, &text(1), limit, None).unwrap();
        assert_eq!(rows(&path).last(), Some(&text(1)));
        assert_eq!(rows(&path).len(), limit);
    }
}

#[test]
fn metrics_collision_and_backup_failure_do_not_change_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let backup = jsonl_file::sibling(&path, ".retention-backup").unwrap();
    let original = b"{\"text\":\"last good\"}\n";
    fs::write(&path, original).unwrap();
    assert!(append(&path, &text(1), 1, Some(&path)).is_err());
    assert!(append(&path, &text(1), 1, Some(&backup)).is_err());
    fs::create_dir(&backup).unwrap();
    assert!(append(&path, &text(1), 1, None).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn large_file_retains_requested_tail_without_accumulating_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut file = File::create(&path).unwrap();
    for n in 0..20_000 { writeln!(file, "{}", text(n)).unwrap(); }
    drop(file);
    append(&path, &text(20_000), 3, None).unwrap();
    assert_eq!(rows(&path), vec![text(19_998), text(19_999), text(20_000)]);
    let mut reader = BufReader::new(File::open(jsonl_file::sibling(&path, ".retention-backup").unwrap()).unwrap());
    let mut row = Vec::new();
    let mut count = 0;
    while read_row(&mut reader, &mut row).unwrap() { assert!(row.capacity() <= MAX_LINE_BYTES); count += 1; }
    assert_eq!(count, 20_000);
}

#[test]
fn concurrent_appends_and_pruning_remain_complete_json_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let threads: Vec<_> = (0..4).map(|worker| {
        let path = path.clone();
        std::thread::spawn(move || { for n in 0..12 { append(&path, &text(worker * 12 + n), 40, None).unwrap(); } })
    }).collect();
    for thread in threads { thread.join().unwrap(); }
    let rows = rows(&path);
    assert_eq!(rows.len(), 40);
    let unique: std::collections::HashSet<_> = rows.iter().map(Value::to_string).collect();
    assert_eq!(unique.len(), 40);
}

#[cfg(windows)]
#[test]
fn windows_replace_denied_preserves_last_good_and_a_recovery_backup() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let original = b"{\"text\":\"last good\"}\n";
    fs::write(&path, original).unwrap();
    let held = fs::OpenOptions::new().read(true).share_mode(3).open(&path).unwrap();
    assert!(append(&path, &text(1), 1, None).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read(jsonl_file::sibling(&path, ".retention-backup").unwrap()).unwrap(), original);
    assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(".wd-write-")));
    drop(held);
    append(&path, &text(1), 1, None).unwrap();
    assert_eq!(rows(&path), vec![text(1)]);
}

#[cfg(unix)]
#[test]
fn pruning_and_backup_do_not_broaden_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"{\"text\":\"private\"}\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    append(&path, &text(1), 1, None).unwrap();
    for file in [&path, &jsonl_file::sibling(&path, ".retention-backup").unwrap()] {
        assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
