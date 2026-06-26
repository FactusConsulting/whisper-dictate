//! Shared OS-cache helpers used by both `audio::model_cache` (feature-gated
//! behind `audio-in-rust`) and `whisper::model_manager` (unconditional).
//!
//! Keeping these at the crate root avoids a cross-module dependency that
//! crosses a feature boundary — `audio` only compiles with `audio-in-rust`,
//! but `whisper::model_manager` must compile in every build.

use std::io;
use std::path::{Path, PathBuf};

/// Rename `tmp` to `target`, replacing `target` if it already exists.
///
/// `std::fs::rename` is atomic on POSIX (replaces the destination in one
/// syscall) but on Windows it fails with `ERROR_ALREADY_EXISTS` when the
/// destination exists. On Windows we delete the destination first and then
/// rename. The small window between the delete and the rename is acceptable
/// because the affected files are one-writer, many-reader artifacts cached
/// once per process run (the Silero ONNX model and GGML whisper models).
pub(crate) fn replace_atomic(tmp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        match std::fs::rename(tmp, target) {
            Ok(()) => Ok(()),
            // Only the "destination exists" case warrants the
            // delete-then-retry dance; unrelated errors (path too long,
            // disk full, permission denied, etc.) must surface as-is.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(target);
                std::fs::rename(tmp, target)
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, target)
    }
}

/// Best-effort OS-conventional user cache directory.
///
/// We avoid the `dirs` crate to keep the dependency surface small — the
/// rules below match its `cache_dir()` on the three platforms we ship on:
/// `%LOCALAPPDATA%` on Windows, `$HOME/Library/Caches` on macOS,
/// `$XDG_CACHE_HOME` (or `$HOME/.cache`) on Linux.
pub(crate) fn user_cache_dir() -> Option<PathBuf> {
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
