use super::*;
use serde_json::json;

fn text(n: usize) -> Value {
    json!({"text": format!("rødgrød-{n}")})
}
fn rows(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn unlimited_is_default_and_never_prunes_or_creates_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"malformed\n").unwrap();
    append(&path, &text(0), 0, None).unwrap();
    assert!(fs::read(&path).unwrap().starts_with(b"malformed\n"));
    assert!(!jsonl_file::sibling(&path, ".retention-backup")
        .unwrap()
        .exists());
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
fn malformed_partial_nonhistory_or_oversized_rows_append_without_pruning_original_or_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let backup = jsonl_file::sibling(&path, ".retention-backup").unwrap();
    fs::write(&backup, b"previous recovery").unwrap();
    for original in [
        b"not json\n".to_vec(),
        b"{\"text\":\"partial".to_vec(),
        b"{\"metrics_only\":true}\n".to_vec(),
        b"[]\n".to_vec(),
        vec![b'x'; MAX_LINE_BYTES + 1],
    ] {
        fs::write(&path, &original).unwrap();
        append(&path, &text(1), 1, None).unwrap();
        let mut expected = original.clone();
        if !expected.ends_with(b"\n") {
            expected.push(b'\n');
        }
        expected.extend(serde_json::to_vec(&text(1)).unwrap());
        expected.push(b'\n');
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(crate::history::last_row(&path).unwrap(), Some(text(1)));
        assert_eq!(fs::read(&backup).unwrap(), b"previous recovery");
    }
}

#[test]
fn repeated_malformed_history_appends_keep_new_rows_readable_and_recovery_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let backup = jsonl_file::sibling(&path, ".retention-backup").unwrap();
    fs::write(&path, b"{\"text\":\"partial").unwrap();
    fs::write(&backup, b"last recovery").unwrap();
    append(&path, &text(1), 1, None).unwrap();
    append(&path, &text(2), 1, None).unwrap();
    let expected = format!("{{\"text\":\"partial\n{}\n{}\n", text(1), text(2));
    assert_eq!(fs::read(&path).unwrap(), expected.as_bytes());
    assert_eq!(crate::history::last_row(&path).unwrap(), Some(text(2)));
    assert_eq!(fs::read(&backup).unwrap(), b"last recovery");
}

#[test]
fn oversized_new_row_still_fails_closed_even_with_malformed_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let original = b"malformed\n";
    fs::write(&path, original).unwrap();
    assert!(append(&path, &json!({"text":"x".repeat(MAX_LINE_BYTES)}), 1, None).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn inspection_io_errors_do_not_fall_back_to_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("marker"), b"untouched").unwrap();
    assert!(append(&path, &text(1), 1, None).is_err());
    assert_eq!(fs::read(path.join("marker")).unwrap(), b"untouched");
    assert!(!jsonl_file::sibling(&path, ".retention-backup")
        .unwrap()
        .exists());
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
fn missing_metrics_parent_is_disjoint_without_creating_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let metrics = dir
        .path()
        .join("not-created")
        .join("nested")
        .join("metrics.jsonl");
    append(&path, &text(0), 1, Some(&metrics)).unwrap();
    append(&path, &text(1), 1, Some(&metrics)).unwrap();
    assert_eq!(rows(&path), vec![text(1)]);
    assert_eq!(
        rows(&jsonl_file::sibling(&path, ".retention-backup").unwrap()),
        vec![text(0)]
    );
    assert!(!dir.path().join("not-created").exists());
}

#[test]
fn missing_metrics_parent_with_traversal_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let metrics = dir
        .path()
        .join("not-created")
        .join("..")
        .join("history.jsonl");
    fs::write(&path, serde_json::to_vec(&text(0)).unwrap()).unwrap();
    assert!(append(&path, &text(1), 1, Some(&metrics)).is_err());
    assert_eq!(rows(&path), vec![text(0)]);
}

#[cfg(unix)]
#[test]
fn dangling_metrics_parent_alias_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let alias = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("missing"), &alias).unwrap();
    fs::write(&path, serde_json::to_vec(&text(0)).unwrap()).unwrap();
    assert!(append(&path, &text(1), 1, Some(&alias.join("metrics.jsonl"))).is_err());
    assert_eq!(rows(&path), vec![text(0)]);
}

#[cfg(unix)]
#[test]
fn existing_metrics_alias_collision_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let alias = dir.path().join("metrics.jsonl");
    fs::write(&path, serde_json::to_vec(&text(0)).unwrap()).unwrap();
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    assert!(append(&path, &text(1), 1, Some(&alias)).is_err());
    assert_eq!(rows(&path), vec![text(0)]);
}

#[test]
fn large_file_retains_requested_tail_without_accumulating_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut file = File::create(&path).unwrap();
    for n in 0..20_000 {
        writeln!(file, "{}", text(n)).unwrap();
    }
    drop(file);
    append(&path, &text(20_000), 3, None).unwrap();
    assert_eq!(rows(&path), vec![text(19_998), text(19_999), text(20_000)]);
    let mut reader = BufReader::new(
        File::open(jsonl_file::sibling(&path, ".retention-backup").unwrap()).unwrap(),
    );
    let mut row = Vec::new();
    let mut count = 0;
    while read_row(&mut reader, &mut row).unwrap() {
        assert!(row.capacity() <= MAX_LINE_BYTES);
        count += 1;
    }
    assert_eq!(count, 20_000);
}

#[test]
fn concurrent_appends_and_pruning_remain_complete_json_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let threads: Vec<_> = (0..4)
        .map(|worker| {
            let path = path.clone();
            std::thread::spawn(move || {
                for n in 0..12 {
                    let event = text(worker * 12 + n);
                    for attempt in 0..10 {
                        match append(&path, &event, 40, None) {
                            Err(error)
                                if error.kind() == io::ErrorKind::TimedOut
                                    && error.to_string()
                                        == "JSONL writer is busy; no row was changed"
                                    && attempt < 9 =>
                            {
                                std::thread::yield_now();
                            }
                            result => {
                                result.unwrap();
                                std::thread::yield_now();
                                break;
                            }
                        }
                    }
                }
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|thread| thread.join()).collect();
    assert!(results.iter().all(Result::is_ok), "a history writer failed");
    let rows = rows(&path);
    assert_eq!(rows.len(), 40);
    let unique: std::collections::HashSet<_> = rows.iter().map(Value::to_string).collect();
    assert_eq!(unique.len(), 40);
}

#[cfg(windows)]
#[test]
fn windows_unicode_case_alias_of_missing_metrics_backup_blocks_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rødgrød.jsonl");
    let metrics = dir.path().join("RØDGRØD.JSONL.RETENTION-BACKUP");
    let original = b"{\"text\":\"keep\"}\n";
    fs::write(&path, original).unwrap();
    let error = append(&path, &text(1), 1, Some(&metrics)).unwrap_err();
    assert!(error.to_string().contains("separate file from metrics"));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!metrics.exists());
}

#[cfg(windows)]
#[test]
fn windows_replace_denied_preserves_last_good_and_a_recovery_backup() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let original = b"{\"text\":\"last good\"}\n";
    fs::write(&path, original).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(append(&path, &text(1), 1, None).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        fs::read(jsonl_file::sibling(&path, ".retention-backup").unwrap()).unwrap(),
        original
    );
    assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".wd-write-")));
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
    for file in [
        &path,
        &jsonl_file::sibling(&path, ".retention-backup").unwrap(),
    ] {
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
