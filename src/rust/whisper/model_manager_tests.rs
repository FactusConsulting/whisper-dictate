//! Unit tests for [`super`] (the `whisper::model_manager` module).
//!
//! Pulled into a sibling file (rather than an inline `mod tests`) to keep
//! `model_manager.rs` under the codebase's 500-LOC cap; wired in via
//! `#[path = "model_manager_tests.rs"] mod tests;` from `model_manager.rs`.
//! The `super::*` glob exposes every private helper the tests touch
//! (`stream_download_to`, `partial_path`, `hex_lower`, `normalize_hex`) under
//! its real name.

use super::*;
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// Counting progress sink used by tests to assert the streaming path
/// actually drives the callback rather than just calling it once at the end.
#[derive(Default)]
struct CountingCb {
    calls: std::sync::atomic::AtomicUsize,
    last_downloaded: std::sync::atomic::AtomicU64,
    last_total: std::sync::Mutex<Option<u64>>,
}

impl DownloadProgress for CountingCb {
    fn on_progress(&self, downloaded: u64, total: Option<u64>) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.last_downloaded
            .store(downloaded, std::sync::atomic::Ordering::Relaxed);
        *self.last_total.lock().unwrap() = total;
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_lower(&h.finalize())
}

#[test]
fn catalog_contains_expected_models() {
    // Encodes the curated scope decision (Wave 7-B): English-only,
    // CPU-friendly. A future PR that drops one of these or accidentally
    // re-orders them will trip this — at which point the UI labels
    // should be re-audited alongside.
    let names: Vec<&str> = CATALOG.iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["tiny.en", "base.en", "small.en"]);
}

#[test]
fn catalog_entries_are_well_formed() {
    for entry in CATALOG {
        assert!(
            !entry.name.is_empty(),
            "catalog name must be non-empty for {entry:?}"
        );
        assert!(
            entry.filename.starts_with("ggml-") && entry.filename.ends_with(".bin"),
            "filename must be ggml-*.bin so it matches whisper.cpp conventions: {entry:?}"
        );
        assert!(
            entry.url.starts_with("https://"),
            "url must be https for {entry:?}"
        );
        assert_eq!(
            entry.sha256.len(),
            64,
            "sha256 must be the 64-char hex digest for {entry:?}"
        );
        assert!(
            entry.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 must be hex for {entry:?}"
        );
        assert!(entry.size_bytes > 0, "size_bytes must be > 0 for {entry:?}");
    }
}

#[test]
fn catalog_names_are_unique() {
    // Two entries with the same `name` would make `find` non-deterministic
    // about which one wins.
    let mut seen = std::collections::HashSet::new();
    for entry in CATALOG {
        assert!(
            seen.insert(entry.name),
            "duplicate catalog name: {}",
            entry.name
        );
    }
}

#[test]
fn find_returns_entry_by_name() {
    let tiny = find("tiny.en").expect("tiny.en in catalog");
    assert_eq!(tiny.filename, "ggml-tiny.en.bin");
    assert_eq!(find("nonsense"), None);
}

#[test]
fn verify_sha256_accepts_matching_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("blob");
    let bytes = b"hello whisper";
    fs::write(&path, bytes).unwrap();
    let expected = sha256_hex(bytes);
    verify_sha256(&path, &expected).expect("matching digest");
}

#[test]
fn verify_sha256_rejects_mismatch_with_helpful_message() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("blob");
    fs::write(&path, b"actual content").unwrap();
    let err = verify_sha256(&path, &"00".repeat(32)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("expected"), "missing expected: {msg}");
    assert!(msg.contains("got"), "missing got: {msg}");
    assert!(msg.contains("00"), "missing expected hex: {msg}");
}

#[test]
fn verify_sha256_accepts_uppercase_expected() {
    // A future catalog edit might accidentally upper-case a digit. The
    // verifier normalises so an otherwise-correct file still passes.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("blob");
    let bytes = b"hello";
    fs::write(&path, bytes).unwrap();
    let expected = sha256_hex(bytes).to_uppercase();
    verify_sha256(&path, &expected).expect("uppercase digest accepted");
}

#[test]
fn verify_sha256_missing_file_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("does-not-exist");
    let err = verify_sha256(&path, &"00".repeat(32)).unwrap_err();
    assert!(err.to_string().contains("open"), "{err}");
}

#[test]
fn stream_download_to_writes_target_and_verifies_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");
    let payload = b"ggml-fake-model-bytes-for-the-test".to_vec();
    let expected = sha256_hex(&payload);
    let cb = CountingCb::default();
    let mut reader = Cursor::new(payload.clone());
    stream_download_to(
        &mut reader,
        Some(payload.len() as u64),
        &partial,
        &target,
        &expected,
        &cb,
    )
    .expect("download succeeds");
    // Final file is in place, partial is gone, bytes match.
    assert!(target.is_file(), "target must exist");
    assert!(!partial.exists(), "partial must be cleaned up via rename");
    assert_eq!(fs::read(&target).unwrap(), payload);
    // Progress was reported at least twice: the initial 0-tick + at
    // least one chunk write.
    assert!(
        cb.calls.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "progress callback should fire on every chunk",
    );
    assert_eq!(
        cb.last_downloaded
            .load(std::sync::atomic::Ordering::Relaxed),
        payload.len() as u64,
        "final progress reading must match total bytes written",
    );
}

#[test]
fn stream_download_to_chunks_drive_progress_repeatedly() {
    // Payload sized so the 64 KiB internal buffer reads it in several
    // chunks — exercises the per-chunk callback path, not just the
    // initial 0-tick + a single read.
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("big.bin.partial");
    let target = tmp.path().join("big.bin");
    let payload = vec![0x55u8; 200 * 1024];
    let expected = sha256_hex(&payload);
    let cb = CountingCb::default();
    let mut reader = Cursor::new(payload.clone());
    stream_download_to(
        &mut reader,
        Some(payload.len() as u64),
        &partial,
        &target,
        &expected,
        &cb,
    )
    .unwrap();
    // 64 KiB chunks for 200 KiB = at least 4 chunk reads + the initial
    // 0-tick.
    assert!(
        cb.calls.load(std::sync::atomic::Ordering::Relaxed) >= 4,
        "expected several per-chunk progress callbacks, got {}",
        cb.calls.load(std::sync::atomic::Ordering::Relaxed),
    );
}

#[test]
fn stream_download_to_rejects_hash_mismatch_and_cleans_partial() {
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");
    let payload = b"some bytes that won't match".to_vec();
    // Deliberately wrong expected digest.
    let bogus = "00".repeat(32);
    let cb = ();
    let mut reader = Cursor::new(payload);
    let err = stream_download_to(&mut reader, None, &partial, &target, &bogus, &cb)
        .expect_err("hash mismatch must error");
    assert!(err.to_string().contains("SHA-256 mismatch"), "{err}");
    assert!(
        !target.exists(),
        "target must NOT be created on hash mismatch",
    );
    assert!(
        !partial.exists(),
        "partial download must be deleted on hash mismatch",
    );
}

#[test]
fn stream_download_to_replaces_existing_target_atomically() {
    // Pre-fix Windows: rename failed when the destination existed. The
    // `replace_atomic` shim deletes-then-renames on AlreadyExists so a
    // redownload over an already-cached (potentially corrupt) file works
    // on every platform.
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");
    fs::write(&target, b"STALE").unwrap();
    let payload = b"fresh download bytes".to_vec();
    let expected = sha256_hex(&payload);
    let mut reader = Cursor::new(payload.clone());
    stream_download_to(&mut reader, None, &partial, &target, &expected, &()).unwrap();
    assert_eq!(fs::read(&target).unwrap(), payload);
}

#[test]
fn partial_path_appends_suffix() {
    let p = partial_path(Path::new("/tmp/ggml-tiny.en.bin"));
    assert_eq!(p, PathBuf::from("/tmp/ggml-tiny.en.bin.partial"));
}

#[test]
fn hex_lower_is_zero_padded() {
    // Smoke check: a single \x01 byte must render as `01`, not `1`.
    assert_eq!(hex_lower(&[0x01, 0xab, 0x00]), "01ab00");
}

#[test]
fn normalize_hex_strips_case_and_whitespace() {
    assert_eq!(normalize_hex("  ABcd  "), "abcd");
}

#[test]
fn is_downloaded_false_for_missing_or_wrong_hash() {
    // Drive `verify_sha256` against a synthetic path we fully control. We
    // avoid touching the real OS cache by computing the path manually
    // instead of routing through `model_path` (which consults
    // `models_cache_dir`).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ggml-fake.bin");
    // Missing file → false (verified via direct `verify_sha256`, since
    // `is_downloaded` calls `model_path` internally).
    let expected = sha256_hex(b"hello");
    assert!(verify_sha256(&path, &expected).is_err());
    // Wrong-content file → mismatch error from `verify_sha256`.
    fs::write(&path, b"different bytes").unwrap();
    assert!(verify_sha256(&path, &expected).is_err());
    // Correct content → ok.
    fs::write(&path, b"hello").unwrap();
    verify_sha256(&path, &expected).expect("matching content");
}
