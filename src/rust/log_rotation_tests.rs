use super::*;

fn permit(_: &Path, _: &Path) -> io::Result<()> {
    Ok(())
}
fn record(writer: &mut RotatingLog, text: &str) -> io::Result<()> {
    writeln!(writer, "{text}")?;
    writer.flush()
}
fn backup(path: &Path) -> PathBuf {
    jsonl_file::sibling(path, ".previous").unwrap()
}

#[test]
fn open_and_empty_flush_do_not_prune_legacy_logs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    fs::write(&path, b"old log larger than configured cap\n").unwrap();
    let original = fs::read(&path).unwrap();
    let mut writer = RotatingLog::open_with(&path, 8, permit).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!backup(&path).exists());
    assert!(RotatingLog::open_with(&path, 0, permit).is_err());
}

#[test]
fn fragmented_unicode_boundary_and_repeated_generation_are_complete() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    let mut writer = RotatingLog::open_with(&path, 12, permit).unwrap();
    let bytes = "rød\n".as_bytes();
    writer.write_all(&bytes[..2]).unwrap();
    writer.write_all(&bytes[2..]).unwrap();
    writer.flush().unwrap();
    record(&mut writer, "second").unwrap();
    assert_eq!(fs::read(&path).unwrap().len(), 12);
    assert!(!backup(&path).exists());
    record(&mut writer, "new").unwrap();
    assert_eq!(fs::read_to_string(backup(&path)).unwrap(), "rød\nsecond\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    record(&mut writer, "another").unwrap();
    record(&mut writer, "last").unwrap();
    assert_eq!(fs::read_to_string(backup(&path)).unwrap(), "new\nanother\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "last\n");
}

#[test]
fn giant_old_log_retains_only_bounded_whole_line_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    for (old, cap, expected) in [
        ("prefixprefix\nrød\nend\n", 9, "rød\nend\n"),
        ("prefixprefix\nrød\nend\n", 8, "end\n"),
        ("a".repeat(200_000).as_str(), 8, ""),
    ] {
        fs::write(&path, old).unwrap();
        let mut writer = RotatingLog::open_with(&path, cap, permit).unwrap();
        record(&mut writer, "next").unwrap();
        assert_eq!(fs::read_to_string(backup(&path)).unwrap(), expected);
        assert!(fs::metadata(backup(&path)).unwrap().len() <= cap as u64);
        assert_eq!(fs::read_to_string(&path).unwrap(), "next\n");
    }
}

#[test]
fn oversized_record_is_discarded_without_publishing_its_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    let mut writer = RotatingLog::open_with(&path, 8, permit).unwrap();
    writer.write_all(b"prefix").unwrap();
    assert!(writer.write_all(b"too long\n").is_err());
    assert!(writer.flush().is_err());
    assert_eq!(fs::read(&path).unwrap(), b"");
    record(&mut writer, "good").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "good\n");
    assert!(writer.pending.is_empty());
}

#[test]
fn backup_failure_preserves_last_good_log_and_retries_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    fs::create_dir(backup(&path)).unwrap();
    let mut writer = RotatingLog::open_with(&path, 9, permit).unwrap();
    assert!(record(&mut writer, "failed").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"lastgood\n");
    fs::remove_dir(backup(&path)).unwrap();
    record(&mut writer, "retry").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"retry\n");
    assert_eq!(fs::read(backup(&path)).unwrap(), b"lastgood\n");
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".wd-write-")));
}

#[test]
fn two_writers_reopen_under_lock_and_keep_unique_complete_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    let a = RotatingLog::open_with(&path, 96, permit).unwrap();
    let b = RotatingLog::open_with(&path, 96, permit).unwrap();
    let handles: Vec<_> = [a, b]
        .into_iter()
        .enumerate()
        .map(|(id, mut writer)| {
            std::thread::spawn(move || {
                for n in 0..50 {
                    record(&mut writer, &format!("{id}:{n:02}")).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    for generation in [&path, &backup(&path)] {
        let bytes = fs::read(generation).unwrap();
        assert!(bytes.len() <= 96);
        assert!(bytes.ends_with(b"\n"));
        for line in String::from_utf8(bytes).unwrap().lines() {
            assert_eq!(line.len(), 4);
            assert!(seen.insert(line.to_owned()), "duplicate {line}");
        }
    }
}

#[cfg(windows)]
#[test]
fn busy_writer_times_out_without_changing_active_or_recovery_then_retries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    let mut writer = RotatingLog::open_with(&path, 9, permit).unwrap();
    let held = jsonl_file::acquire(&path).unwrap();
    assert!(record(&mut writer, "failed").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"lastgood\n");
    assert!(!backup(&path).exists());
    drop(held);
    record(&mut writer, "retry").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"retry\n");
}

#[cfg(windows)]
#[test]
fn windows_reader_without_delete_share_preserves_active_and_recovery() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    let mut writer = RotatingLog::open_with(&path, 9, permit).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(record(&mut writer, "failed").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"lastgood\n");
    assert_eq!(fs::read(backup(&path)).unwrap(), b"lastgood\n");
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".wd-write-")));
    drop(held);
    record(&mut writer, "retry").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"retry\n");
}

#[cfg(unix)]
#[test]
fn recovery_is_private_and_redirected_backup_is_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    fs::write(&path, b"lastgood\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut writer = RotatingLog::open_with(&path, 9, permit).unwrap();
    record(&mut writer, "retry").unwrap();
    assert_eq!(
        fs::metadata(backup(&path)).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_file(backup(&path)).unwrap();
    let other = dir.path().join("private");
    fs::write(&other, b"do not touch").unwrap();
    symlink(&other, backup(&path)).unwrap();
    assert!(record(&mut writer, "last").is_err());
    assert_eq!(fs::read(&other).unwrap(), b"do not touch");
    assert_eq!(fs::read(&path).unwrap(), b"retry\n");
}
