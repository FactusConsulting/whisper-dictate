use super::{write, write_private, write_with, write_with_replace, Permissions};
use std::fs;
use std::io::{self, Write};

#[test]
fn removal_syncs_the_parent_after_unlink_and_reports_sync_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    fs::write(&path, b"synthetic credential").unwrap();
    let error = super::remove_with_sync(&path, |parent| {
        assert_eq!(parent, dir.path());
        assert!(!path.exists(), "sync must follow removal");
        Err(io::Error::other("synthetic directory-sync failure"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("directory-sync failure"));
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn credential_removal_follows_a_symlink_without_removing_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    let target = dir.path().join("actual-keys.json");
    fs::write(&target, b"synthetic credential").unwrap();
    std::os::unix::fs::symlink("actual-keys.json", &path).unwrap();
    super::remove(&path).unwrap();
    assert!(!target.exists());
    assert!(fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
}

fn assert_no_temporary_files(directory: &std::path::Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".wd-write-")
    }));
}

#[test]
fn atomic_replacement_creates_then_replaces_complete_utf8_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings with spaces.json");
    write(&path, br#"{"unknown":true,"lang":null}"#).unwrap();
    let next = "{\"unknown\":true,\"text\":\"dansk: æøå\"}\n";
    write(&path, next.as_bytes()).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), next);
    assert_no_temporary_files(dir.path());
}

#[test]
fn partial_write_failure_keeps_last_good_bytes_and_removes_owned_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let previous = br#"{"known":"last good","unknown":true,"lang":null}"#;
    fs::write(&path, previous).unwrap();
    let error = write_with(&path, Permissions::Preserve, |file| {
        file.write_all(b"partial invalid JSON")?;
        Err(io::Error::other("synthetic write failure"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("synthetic write failure"));
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert_no_temporary_files(dir.path());
}

#[test]
fn non_file_destination_is_rejected_without_touching_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("last-good.json"), b"{}").unwrap();
    assert!(write(&path, b"[]").is_err());
    assert_eq!(fs::read(path.join("last-good.json")).unwrap(), b"{}");
    assert_no_temporary_files(dir.path());
}

#[test]
fn replacement_failure_keeps_last_good_json_and_removes_the_new_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let previous = br#"{"last_good":true}"#;
    fs::write(&path, previous).unwrap();
    let error = write_with_replace(
        &path,
        Permissions::Preserve,
        |file| file.write_all(br#"{"next":true}"#),
        |from, to| {
            assert_eq!(fs::read(from)?, br#"{"next":true}"#);
            assert_eq!(fs::read(to)?, previous);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic replacement failure",
            ))
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(path).unwrap(), previous);
    assert_no_temporary_files(dir.path());
}

#[cfg(unix)]
#[test]
fn atomic_user_state_preserves_mode_and_private_files_are_always_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    write(&path, b"[]").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    write_private(&path, b"{\"key\":\"synthetic\"}").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    write_with(&path, Permissions::Private, |file| {
        assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
        file.write_all(b"{}")
    })
    .unwrap();
    assert_no_temporary_files(dir.path());
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_a_symlink_and_replaces_its_target() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("actual.json");
    let link = dir.path().join("config.json");
    fs::write(&target, "{}").unwrap();
    symlink(&target, &link).unwrap();
    write(&link, b"[]").unwrap();
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(target).unwrap(), b"[]");
    assert_no_temporary_files(dir.path());
}

#[cfg(unix)]
#[test]
fn atomic_first_save_follows_dangling_relative_symlink_chains() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("config.json");
    let intermediate = dir.path().join("other.json");
    let target = dir.path().join("actual.json");
    symlink("other.json", &link).unwrap();
    symlink("actual.json", &intermediate).unwrap();
    write(&link, b"[]").unwrap();
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(&intermediate)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(target).unwrap(), b"[]");
    assert_no_temporary_files(dir.path());
}

#[cfg(unix)]
#[test]
fn atomic_save_keeps_a_read_only_destination_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-only.json");
    fs::write(&path, b"last good").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    // Root may write this file even through the previous non-atomic path.
    if fs::OpenOptions::new().write(true).open(&path).is_ok() {
        return;
    }
    assert_eq!(
        write(&path, b"changed").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read(&path).unwrap(), b"last good");
    assert_no_temporary_files(dir.path());
}

#[cfg(unix)]
#[test]
fn private_file_mode_is_restored_under_a_restrictive_umask() {
    // Change the umask only in a child process, not the parallel test harness.
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("sh")
        .args(["-c", "umask 0477; exec \"$WD_ATOMIC_TEST_EXE\" --exact atomic_file::tests::private_file_umask_child --nocapture"])
        .env("WD_ATOMIC_TEST_EXE", std::env::current_exe().unwrap())
        .env("WD_ATOMIC_UMASK_PATH", dir.path().join("keys.json"))
        // This child deliberately removes owner-read permissions from newly
        // created files. Keep its profiler output outside the parent's coverage
        // merge inputs; production paths are covered by the ordinary tests.
        .env("LLVM_PROFILE_FILE", dir.path().join("umask-child.profraw"))
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_no_temporary_files(dir.path());
}

#[cfg(unix)]
#[test]
fn private_file_umask_child() {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("WD_ATOMIC_UMASK_PATH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    write_private(&path, b"synthetic").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(&path).unwrap(), b"synthetic");
}

#[cfg(windows)]
#[test]
fn windows_locked_destination_keeps_last_good_json_and_cleans_temporary() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, br#"{"kept":true}"#).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(write_private(&path, b"partial invalid JSON").is_err());
    drop(lock);
    assert_eq!(fs::read(&path).unwrap(), br#"{"kept":true}"#);
    assert_no_temporary_files(dir.path());
}
