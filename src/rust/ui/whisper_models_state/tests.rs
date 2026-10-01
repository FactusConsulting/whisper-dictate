use super::*;

// ── spawn_download tests ──────────────────────────────────────────────────

use crate::test_env_lock::ENV_LOCK;
use std::ffi::OsString;

/// Save/restore wrapper for env-var mutation in tests. Mirrors the pattern
/// in `model_manager_tests.rs` — defined inline so we don't need a
/// `pub(super)` dep on `ui::test_support`.
struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Platform-specific env var that controls the OS user-cache directory,
/// mirroring `model_manager::user_cache_dir`'s resolution order.
const CACHE_ENV_VAR: &str = if cfg!(windows) {
    "LOCALAPPDATA"
} else if cfg!(target_os = "macos") {
    "HOME"
} else {
    "XDG_CACHE_HOME"
};

#[path = "download_tests.rs"]
mod download;

#[path = "verification_tests.rs"]
mod verification;
