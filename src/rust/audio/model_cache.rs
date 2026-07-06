//! Where the bundled Silero ONNX model lives on disk at runtime.
//!
//! `vad-rs::Vad::new` accepts only a filesystem path, so the embedded
//! `silero_vad.onnx` bytes have to be materialised somewhere. The
//! original implementation used `tempfile::NamedTempFile::keep()`,
//! which leaked a fresh model file into `%TEMP%` on every process run —
//! never cleaned up because the `NamedTempFile` handle was deliberately
//! dropped before its destructor could delete the file.
//!
//! This module replaces that with a stable, version-pinned cache under
//! the OS user-cache directory and falls back to the original tempfile
//! path only when the cache is unavailable. Lifecycle is documented on
//! [`cache_or_temp_model_path`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::os_cache::{replace_atomic, user_cache_dir};

/// Process-local counter that gives every call to
/// [`cache_or_temp_model_path`] a unique tmp filename. Without this,
/// two concurrent calls (e.g. two whisper-dictate processes racing on
/// first-run model materialization, or two tests running in parallel
/// inside the same test binary) collide on the fixed `.partial` path:
/// the second `File::create` truncates the first caller's tmp inode to
/// zero bytes before the first caller's `replace_atomic` renames it
/// into the cache target, so the cached file ends up empty.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Resolve where to materialise the bundled Silero ONNX model on disk.
///
/// Preferred path: the OS user-cache directory under `whisper-dictate/`.
/// We write the bytes once on first call and re-use the same path on
/// subsequent calls (a length sanity check refreshes the file when the
/// embedded bytes change — typically on version upgrade). The file is
/// intentionally **not** deleted on drop because the next process run
/// reuses it; this is a fixed-size, version-pinned artifact, not
/// transient data.
///
/// Fallback path: if the cache dir can't be located OR we can't write
/// to it, extract to a [`tempfile::NamedTempFile`] and `keep()` the
/// path. That leaks a single file into the temp dir until process exit,
/// which is the previous behaviour; the OS sweeps the temp dir.
pub(crate) fn cache_or_temp_model_path(model_bytes: &[u8]) -> Result<PathBuf, anyhow::Error> {
    use std::io::Write;
    if let Some(cache_dir) = user_cache_dir().map(|d| d.join("whisper-dictate")) {
        if std::fs::create_dir_all(&cache_dir).is_ok() {
            let target = cache_dir.join("silero_vad.onnx");
            // Only rewrite if absent or size-mismatched. Avoids touching
            // the file (and any AV scanner that's watching it) on hot
            // launches; a real bytes mismatch shows up as a size delta
            // because the model bytes are an embedded version-pinned
            // artifact.
            let needs_write = match std::fs::metadata(&target) {
                Ok(meta) => meta.len() != model_bytes.len() as u64,
                Err(_) => true,
            };
            if !needs_write {
                return Ok(target);
            }
            // Write atomically via a sibling temp file so a crashed
            // half-write never leaves a corrupt model in place. Per-call
            // unique filename (process id + atomic counter) so concurrent
            // callers don't truncate each other's tmp inode in the window
            // between `File::create` and `replace_atomic`.
            let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let tmp = cache_dir.join(format!(
                ".silero_vad.onnx.partial.{}.{}",
                std::process::id(),
                counter,
            ));
            if let Ok(mut f) = std::fs::File::create(&tmp) {
                if f.write_all(model_bytes).is_ok() && f.flush().is_ok() {
                    drop(f);
                    if replace_atomic(&tmp, &target).is_ok() {
                        return Ok(target);
                    }
                }
                // Best-effort cleanup; ignore failure since we're about
                // to fall through to the tempfile path anyway.
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
    // Fallback: tempfile that outlives the NamedTempFile handle via
    // keep(). One leaked file per process is acceptable when this
    // branch fires (locked-down sandboxes, etc.).
    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(model_bytes)?;
    tmp.flush()?;
    let (_file, path) = tmp.keep()?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Use the CRATE-WIDE env lock, not a module-local one. The Rust 2024
    // `unsafe` contract on `set_var` / `remove_var` requires no concurrent
    // reader anywhere in the process — a module-local lock cannot guarantee
    // that against env-mutating tests in OTHER modules (config, runtime, ui).
    // Codex flagged the previous module-local lock as unsound for exactly
    // this reason (PR #340 iteration-2 finding #1). See
    // `crate::test_env_lock` for the full contract.
    use crate::os_cache::{replace_atomic, user_cache_dir};
    use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

    /// Platform-specific env var that controls the OS user-cache directory,
    /// mirroring `crate::os_cache::user_cache_dir`'s resolution order.
    const CACHE_ENV_VAR: &str = if cfg!(windows) {
        "LOCALAPPDATA"
    } else if cfg!(target_os = "macos") {
        "HOME"
    } else {
        "XDG_CACHE_HOME"
    };

    /// Iteration-2 review finding #2: on Windows, `std::fs::rename`
    /// fails when the destination already exists, so a re-run of
    /// `cache_or_temp_model_path` with a different-sized embedded
    /// model could not overwrite the stale file. The fix routes
    /// renames through `replace_atomic`, which on Windows retries
    /// after `remove_file(target)`.
    ///
    /// This test pre-creates a stale file at the would-be cache path,
    /// invokes `cache_or_temp_model_path`, and asserts the returned
    /// file contains the NEW bytes (not the stale ones). It runs on
    /// every platform because the new helper is exercised on every
    /// platform; on Linux/macOS `rename` would have replaced the
    /// stale file anyway, but the assertion still holds and guards
    /// against any future refactor that bypasses `replace_atomic`.
    #[test]
    fn cache_replaces_stale_file_with_new_bytes() {
        // Take the env lock before touching any process-global
        // variable; held across the entire test so the cache resolver
        // reads our scratch values, not whatever another test set.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Point the cache resolver at a per-test scratch dir so we
        // don't touch the real user cache. The expected on-disk path
        // is computed by the resolver itself so this test stays
        // honest if `user_cache_dir`'s OS conventions change.
        let tmp_root = tempfile::tempdir().expect("create temp dir");
        let scratch = tmp_root.path().to_path_buf();
        // RAII guard restores the original cache-dir env var on Drop —
        // including on panic. The previous `override_cache_env / …
        // restore_cache_env` pair would never run on panic, leaking the
        // scratch dir into every later test.
        let _cache_env = EnvVarGuard::set(CACHE_ENV_VAR, &scratch);

        // Resolve the cache dir THIS test will use, mirroring the
        // production logic, and seed a stale file there with bytes of
        // a DIFFERENT length so the size-mismatch `needs_write` path
        // is taken.
        let cache_dir = user_cache_dir()
            .map(|d| d.join("whisper-dictate"))
            .expect("scratch env must resolve to a cache dir");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        let target = cache_dir.join("silero_vad.onnx");
        let stale_bytes = b"STALE-MODEL-PAYLOAD-DIFFERENT-LENGTH".to_vec();
        std::fs::write(&target, &stale_bytes).expect("seed stale file");

        let new_bytes = b"FRESH-MODEL-BYTES-WHICH-DIFFER-IN-CONTENT-AND-SIZE".to_vec();
        assert_ne!(
            stale_bytes.len(),
            new_bytes.len(),
            "test must use bytes of different length so the size-mismatch \
             needs_write check fires",
        );
        let result = cache_or_temp_model_path(&new_bytes).expect("cache path");

        // The returned path must point at our scratch cache (not the
        // tempfile fallback), and the file contents must be the NEW
        // bytes — proving the stale file was replaced. On pre-fix
        // Windows, the rename would fail and the fallback path under
        // %TEMP% would be returned instead, failing the assert_eq.
        assert_eq!(
            result, target,
            "expected cache path under the scratch dir, got {result:?}",
        );
        let on_disk = std::fs::read(&target).expect("read cached file");
        assert_eq!(
            on_disk, new_bytes,
            "stale cache must be replaced with new bytes (Windows rename bug)",
        );
    }

    /// Regression for iteration-2 review finding #2 on PR #340: the Windows
    /// branch of `replace_atomic` was previously a blanket retry that swallowed
    /// every error and re-ran after `remove_file(target)`. It now retries
    /// ONLY on `ErrorKind::AlreadyExists` and surfaces every other error
    /// untouched, so unrelated failures (permission denied, path too long,
    /// disk full, etc.) reach the caller with their real diagnostic AND
    /// without a spurious delete of the target file as a side effect.
    ///
    /// We trigger a NotFound by renaming from a path that doesn't exist on
    /// disk; the kernel surfaces that consistently on all platforms, and on
    /// Windows specifically it goes through the new narrowed match arm. We
    /// then assert (a) the returned error is NOT AlreadyExists, and (b) the
    /// pre-existing target file is still on disk with its original bytes —
    /// proving the no-retry / no-remove-file behaviour.
    #[test]
    fn replace_atomic_passes_through_non_already_exists_errors() {
        let tmp_root = tempfile::tempdir().expect("create temp dir");
        // Source path that DOES NOT exist on disk — `rename` returns
        // `ErrorKind::NotFound`, exercising the non-AlreadyExists branch.
        let missing_tmp = tmp_root.path().join(".does-not-exist.partial");
        assert!(
            !missing_tmp.exists(),
            "test precondition: tmp path must not exist",
        );

        // Pre-existing target with sentinel bytes so we can detect if the
        // (now-removed) blanket retry path snuck back in and deleted it.
        let target = tmp_root.path().join("silero_vad.onnx");
        let sentinel = b"PRE-EXISTING-TARGET-MUST-SURVIVE".to_vec();
        std::fs::write(&target, &sentinel).expect("seed target");

        let err = replace_atomic(&missing_tmp, &target)
            .expect_err("rename from missing source must fail");

        assert_ne!(
            err.kind(),
            std::io::ErrorKind::AlreadyExists,
            "non-AlreadyExists errors must surface as-is, not be remapped",
        );
        // The old blanket-retry implementation would have called
        // `remove_file(&target)` and then re-failed; the new code returns
        // the original error without touching the target. Verify both by
        // re-reading the bytes.
        assert!(
            target.exists(),
            "target must NOT be deleted on non-AlreadyExists error"
        );
        let on_disk = std::fs::read(&target).expect("read target after failed rename");
        assert_eq!(
            on_disk, sentinel,
            "target bytes must be untouched when replace_atomic fails with non-AlreadyExists",
        );
    }
}
