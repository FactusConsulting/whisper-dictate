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
                    if std::fs::rename(&tmp, &target).is_ok() {
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
