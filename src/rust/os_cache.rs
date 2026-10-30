//! Shared OS-cache helpers used by `whisper::model_manager`.

use std::path::PathBuf;

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
