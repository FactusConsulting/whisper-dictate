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
use std::io::{self, Cursor};

// Re-export the crate-wide env lock + guard so the cache-dir override tests
// serialise against every other env-mutating test in the suite. Per-module
// locks/guards would violate the soundness contract `env::set_var` requires
// under the Rust 2024 edition (see `crate::test_env_lock`).
use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

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
        // All lowercase: normalize_hex lowercases on verify, but the catalog
        // itself should already be lowercase so a typo is caught here rather
        // than silently passing the normalised comparison.
        assert_eq!(
            entry.sha256,
            entry.sha256.to_ascii_lowercase(),
            "catalog sha256 must be lowercase for {entry:?}"
        );
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
fn partial_path_is_unique_and_has_partial_suffix() {
    let target = Path::new("/tmp/ggml-tiny.en.bin");
    let p1 = partial_path(target);
    let p2 = partial_path(target);
    let s1 = p1.to_string_lossy();
    let s2 = p2.to_string_lossy();
    assert!(
        s1.starts_with("/tmp/ggml-tiny.en.bin."),
        "partial must start with target prefix: {s1}"
    );
    assert!(
        s1.ends_with(".partial"),
        "partial must end with .partial: {s1}"
    );
    // The sequence counter guarantees uniqueness within the same process so
    // concurrent downloads (CLI + UI) never stomp the same partial file.
    assert_ne!(s1, s2, "consecutive calls must produce different paths");
}

#[test]
fn is_local_only_reads_env_var() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g1 = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
    assert!(is_local_only(), "\"1\" must count as local-only");
    drop(_g1);
    let _g2 = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "true");
    assert!(is_local_only(), "\"true\" must count as local-only");
    drop(_g2);
    let _g3 = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    assert!(!is_local_only(), "unset must not be local-only");
    drop(_g3);
    let _g4 = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "0");
    assert!(!is_local_only(), "\"0\" must not be local-only");
}

#[test]
fn download_model_blocked_when_local_only_via_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
    // Also ensure config doesn't interfere by pointing config at empty dir.
    let tmp_cfg = tempfile::tempdir().unwrap();
    let cfg_path = tmp_cfg.path().join("config.json");
    let _gcfg = EnvVarGuard::set("VOICEPI_CONFIG", cfg_path.to_str().unwrap());
    let entry = &CATALOG[0];
    let err = download_model(entry, &()).expect_err("must fail in local-only mode");
    assert!(
        err.to_string().contains("local-only mode"),
        "error must mention local-only mode: {err}"
    );
}

#[test]
fn download_model_blocked_when_local_only_via_config() {
    // P1: persisted `local_only: 1` in settings.json must block downloads
    // even without the VOICEPI_LOCAL_ONLY env var.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g_env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let tmp_cfg = tempfile::tempdir().unwrap();
    let cfg_path = tmp_cfg.path().join("config.json");
    std::fs::write(&cfg_path, r#"{"local_only":"1"}"#).unwrap();
    let _gcfg = EnvVarGuard::set("VOICEPI_CONFIG", cfg_path.to_str().unwrap());
    let entry = &CATALOG[0];
    let err = download_model(entry, &()).expect_err("must fail when config sets local_only");
    assert!(
        err.to_string().contains("local-only mode"),
        "error must mention local-only mode: {err}"
    );
}

#[test]
fn is_local_only_reads_persisted_config() {
    // P1: when env var is unset but config.json has "local_only":"1",
    // is_local_only() must return true.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g_env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let tmp_cfg = tempfile::tempdir().unwrap();
    let cfg_path = tmp_cfg.path().join("config.json");
    std::fs::write(&cfg_path, r#"{"local_only":"1"}"#).unwrap();
    let _gcfg = EnvVarGuard::set("VOICEPI_CONFIG", cfg_path.to_str().unwrap());
    assert!(
        is_local_only(),
        "config local_only=1 must activate local-only mode even without env var"
    );
}

#[test]
fn download_timeout_uses_default_when_env_absent() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::remove("VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS");
    assert_eq!(
        download_timeout(),
        std::time::Duration::from_secs(DEFAULT_DOWNLOAD_TIMEOUT_SECS),
        "default timeout must be used when env var is unset"
    );
}

#[test]
fn download_timeout_env_override_is_respected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvVarGuard::set("VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS", "7200");
    assert_eq!(
        download_timeout(),
        std::time::Duration::from_secs(7200),
        "env override must be used when set"
    );
    drop(_g);
    // Garbage value falls back to default.
    let _g2 = EnvVarGuard::set("VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS", "not-a-number");
    assert_eq!(
        download_timeout(),
        std::time::Duration::from_secs(DEFAULT_DOWNLOAD_TIMEOUT_SECS),
        "invalid env override must fall back to default"
    );
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

/// Pick the env var `user_cache_dir` consults on the current platform so
/// the override tests pin the resolution to a temp dir regardless of host.
const CACHE_ENV_VAR: &str = if cfg!(windows) {
    "LOCALAPPDATA"
} else if cfg!(target_os = "macos") {
    "HOME"
} else {
    "XDG_CACHE_HOME"
};

/// On Linux `user_cache_dir` also consults HOME as the fallback after
/// XDG_CACHE_HOME. Tests that "clear all sources" must wipe both so the
/// fallback doesn't accidentally pick up the developer's real HOME.
const SECONDARY_CACHE_ENV_VAR: Option<&str> = if cfg!(windows) || cfg!(target_os = "macos") {
    None
} else {
    Some("HOME")
};

#[test]
fn models_cache_dir_resolves_under_overridden_root() {
    // Pin the OS cache dir under tempdir so the test asserts the EXACT
    // layout (`<base>/whisper-dictate/whisper-models`) without touching
    // the developer's real cache. Covers the `models_cache_dir`,
    // `model_path` and `user_cache_dir` happy paths in one shot.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path());
    // On macOS `user_cache_dir` derives its path by appending
    // `Library/Caches` to HOME; account for that so the assertion holds
    // on every platform.
    let expected_base: PathBuf = if cfg!(target_os = "macos") {
        tmp.path().join("Library/Caches")
    } else {
        tmp.path().to_path_buf()
    };
    let dir = models_cache_dir().expect("override must resolve");
    assert_eq!(
        dir,
        expected_base.join("whisper-dictate").join("whisper-models")
    );

    let entry = &CATALOG[0];
    let path = model_path(entry).expect("override must resolve");
    assert_eq!(path, dir.join(entry.filename));
}

#[test]
fn is_downloaded_false_when_cache_is_empty() {
    // End-to-end exercise of `is_downloaded`'s real plumbing (model_path
    // → file probe → verify_sha256), under a pinned temp cache so the
    // result is deterministic regardless of what's on the developer's
    // disk. The directory deliberately doesn't exist (`tempdir` returns
    // an empty dir; the `whisper-dictate/whisper-models` subpath has not
    // been created yet) so the `is_file()` branch returns false.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path());
    for entry in CATALOG {
        assert!(
            !is_downloaded(entry),
            "fresh tempdir cache must report {} as not downloaded",
            entry.name,
        );
    }
}

#[test]
fn is_downloaded_true_when_cached_file_matches_hash() {
    // Plant a synthetic GGML payload whose SHA-256 we forge into a
    // throwaway `ModelEntry`, then assert `is_downloaded` flips to true.
    // We can't mutate the real CATALOG entry's hash (it's `&'static`),
    // so the test builds its own entry pointing at the same filename and
    // routes via `model_path` to honour the OS layout.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path());
    let payload = b"forged-ggml-bytes-just-for-this-test".to_vec();
    let digest = sha256_hex(&payload);
    // Leak the strings so they satisfy the `&'static str` field types.
    let hash_static: &'static str = Box::leak(digest.into_boxed_str());
    let entry = ModelEntry {
        name: "test-only",
        filename: "ggml-test-only.bin",
        url: "https://example.invalid/",
        sha256: hash_static,
        size_bytes: payload.len() as u64,
        description: "synthetic test entry",
    };
    let path = model_path(&entry).expect("override must resolve");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &payload).unwrap();
    assert!(
        is_downloaded(&entry),
        "synthetic cached payload must report as downloaded",
    );
}

#[test]
fn models_cache_dir_errors_when_no_env_resolvable() {
    // Cover the `ok_or_else` branch: with every cache env-var source
    // cleared, `user_cache_dir` returns None and `models_cache_dir`
    // bubbles a helpful error instead of panicking.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _primary = EnvVarGuard::remove(CACHE_ENV_VAR);
    let _secondary = SECONDARY_CACHE_ENV_VAR.map(EnvVarGuard::remove);
    match models_cache_dir() {
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("cache directory"),
                "error must mention cache directory: {msg}",
            );
        }
        Ok(dir) => {
            // On a developer machine some env var we didn't think of
            // (or a per-process inherited value) may still resolve.
            // Don't fail the suite on that — but at least sanity-check
            // the returned shape.
            assert!(dir.ends_with("whisper-models"));
        }
    }
}

/// `Read` impl that returns Ok with `payload_len` bytes once and then
/// fails on the next read. Used to drive `stream_download_to` into its
/// per-chunk error branch (the `match reader.read(...)` Err arm).
struct OneOkThenFailReader {
    payload: Vec<u8>,
    served: bool,
}

impl Read for OneOkThenFailReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.served {
            return Err(io::Error::other("synthetic read fail"));
        }
        let n = buf.len().min(self.payload.len());
        buf[..n].copy_from_slice(&self.payload[..n]);
        self.served = true;
        Ok(n)
    }
}

#[test]
fn stream_download_to_propagates_read_error_and_cleans_partial() {
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");
    let mut reader = OneOkThenFailReader {
        payload: b"a few bytes".to_vec(),
        served: false,
    };
    let err = stream_download_to(&mut reader, None, &partial, &target, &"00".repeat(32), &())
        .expect_err("read error must propagate");
    let msg = err.to_string();
    assert!(
        msg.contains("download read failed"),
        "error must mention the read failure: {msg}",
    );
    assert!(
        !target.exists(),
        "target must NOT be created on read failure",
    );
    assert!(
        !partial.exists(),
        "partial must be cleaned up after a read failure",
    );
}

#[test]
fn stream_download_to_errors_when_partial_parent_missing() {
    // Drives the `File::create(partial)` failure path: the partial path
    // lives under a directory that does not exist on disk, so the
    // create fails before any bytes are read. The error is surfaced
    // verbatim (no partial cleanup is needed because no file ever
    // existed).
    let tmp = tempfile::tempdir().unwrap();
    let nonexistent_dir = tmp.path().join("definitely-not-here");
    let partial = nonexistent_dir.join("model.bin.partial");
    let target = nonexistent_dir.join("model.bin");
    let payload = b"unused".to_vec();
    let expected = sha256_hex(&payload);
    let mut reader = Cursor::new(payload);
    let err = stream_download_to(&mut reader, None, &partial, &target, &expected, &())
        .expect_err("missing parent must error");
    assert!(
        err.to_string().contains("failed to create"),
        "error must mention create failure: {err}",
    );
    assert!(!partial.exists(), "no partial must be left behind");
    assert!(!target.exists(), "no target must be created");
}
