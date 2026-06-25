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
            // half-write never leaves a corrupt model in place.
            let tmp = cache_dir.join(".silero_vad.onnx.partial");
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

/// Rename `tmp` to `target`, replacing `target` if it already exists.
///
/// `std::fs::rename` is atomic on POSIX (replaces the destination in
/// one syscall) but on Windows it FAILS with `ERROR_ALREADY_EXISTS`
/// when the destination exists. That broke the model-upgrade path on
/// Windows: a re-run with a different-sized embedded model would write
/// the partial file, fail to rename, and fall through to the
/// tempfile-leak path (PR #335 iteration-2 review finding #2). To
/// recover, on Windows we delete the destination first and then
/// rename. The small window between the delete and the rename is fine
/// here because the cached file is read at most once per process at
/// startup — a concurrent reader on the same machine is not part of
/// our model.
fn replace_atomic(tmp: &std::path::Path, target: &std::path::Path) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        match std::fs::rename(tmp, target) {
            Ok(()) => Ok(()),
            Err(_) => {
                // The error kind on Windows for "destination exists" is
                // unstable across stdlib versions; treat any rename
                // failure as "destination is in the way" and retry
                // after deleting. If `remove_file` ALSO fails (file
                // doesn't exist, locked, etc.) the second rename's
                // error is what the caller sees, which is the most
                // informative diagnostic.
                let _ = std::fs::remove_file(target);
                std::fs::rename(tmp, target)
            }
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, target)
    }
}

/// Best-effort OS-conventional user cache directory. We avoid the
/// `dirs` crate to keep the dependency surface small — the rules below
/// match its `cache_dir()` on the three platforms we ship on:
/// `%LOCALAPPDATA%` on Windows, `$HOME/Library/Caches` on macOS,
/// `$XDG_CACHE_HOME` (or `$HOME/.cache`) on Linux.
fn user_cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return Some(PathBuf::from(xdg));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Point the cache resolver at a per-test scratch dir so we
        // don't touch the real user cache. The expected on-disk path
        // is computed by the resolver itself so this test stays
        // honest if `user_cache_dir`'s OS conventions change.
        let tmp_root = tempfile::tempdir().expect("create temp dir");
        let scratch = tmp_root.path().to_path_buf();
        let prev_env = override_cache_env(&scratch);

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

        restore_cache_env(prev_env);

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

    /// Override the cache-dir env var to the test scratch dir,
    /// returning the previous value for later restoration. The dir
    /// is chosen so that `user_cache_dir()` returns a sub-path of
    /// `scratch` on every supported platform.
    fn override_cache_env(scratch: &std::path::Path) -> Option<std::ffi::OsString> {
        #[cfg(windows)]
        let var = "LOCALAPPDATA";
        #[cfg(target_os = "macos")]
        let var = "HOME";
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let var = "XDG_CACHE_HOME";

        let prev = std::env::var_os(var);
        std::env::set_var(var, scratch);
        prev
    }

    fn restore_cache_env(prev: Option<std::ffi::OsString>) {
        #[cfg(windows)]
        let var = "LOCALAPPDATA";
        #[cfg(target_os = "macos")]
        let var = "HOME";
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let var = "XDG_CACHE_HOME";
        match prev {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }
}
