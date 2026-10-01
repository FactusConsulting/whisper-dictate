use crate::atomic_file::{write, write_private};
use std::fs;

#[test]
fn named_stream_destination_is_rejected_without_changing_either_stream() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let stream = directory.path().join("config.json:wd-fixture");
    fs::write(&path, b"last good").unwrap();
    fs::write(&stream, b"metadata").unwrap();
    let error = write(&path, b"next").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(fs::read(&path).unwrap(), b"last good");
    assert_eq!(fs::read(&stream).unwrap(), b"metadata");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn low_integrity_label_is_preserved_on_a_private_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keys.json");
    fs::write(&path, b"synthetic").unwrap();
    let output = std::process::Command::new("icacls.exe")
        .arg(&path)
        .args(["/setintegritylevel", "L"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (descriptor, label) = super::label_descriptor(&path).unwrap();
    let before = super::label_bytes(label);
    assert!(
        !before.is_empty(),
        "fixture must have an explicit integrity label"
    );
    drop(descriptor);
    write_private(&path, b"next synthetic").unwrap();
    let (descriptor, label) = super::label_descriptor(&path).unwrap();
    assert_eq!(super::label_bytes(label), before);
    drop(descriptor);
    assert_eq!(fs::read(&path).unwrap(), b"next synthetic");
}
