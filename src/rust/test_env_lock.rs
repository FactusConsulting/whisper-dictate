//! Process-wide serialisation for tests that mutate environment variables.
//!
//! Cargo runs the library unit tests in parallel inside a single binary, and
//! `std::env::set_var` / `remove_var` mutate process-global state. Holding a
//! lock around each override/restore window keeps the writes from racing the
//! stdlib's own env reads in unrelated tests.
//!
//! Under the Rust 2024 edition `set_var` / `remove_var` are `unsafe`: the
//! caller asserts there is no concurrent reader in the entire process. A
//! **module-local** lock cannot discharge that obligation because a test in a
//! different module might be reading or writing the same variable behind its
//! own lock. The only sound design is a single crate-wide lock that every
//! env-mutating test takes — that is what this module is for.
//!
//! ## Usage rule
//!
//! Every `#[test]` in the library that mutates an env var (directly or via
//! [`EnvVarGuard`]) MUST hold [`ENV_LOCK`] across the override/restore
//! window. Prefer [`EnvVarGuard`] over manual `set_var`/`remove_var` pairs
//! because the guard restores the original value on Drop — so a panic in
//! the middle of a test does not leak the override into every later test
//! in the same library binary (Codex P2 #415 pattern). Re-export this
//! module's items from per-module `test_support` shims rather than defining
//! local copies — historical per-module guards/locks were consolidated here
//! exactly because they could not serialise against each other.
//!
//! Integration tests in `tests/` live in their own binaries and so do not
//! need to share this lock with the library suite.

use std::env;
use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

/// The single crate-wide guard serialising env-mutating tests. See the module
/// docs for the soundness contract.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that snapshots an env var on construction, mutates it to the
/// requested state, and restores the original value (or absence) on Drop.
///
/// Callers MUST hold [`ENV_LOCK`] for the guard's entire lifetime so the
/// mutation does not race other env-mutating tests in the same library
/// binary. The guard captures the previous value at construction time and
/// restores it unconditionally on Drop — so a panic inside the test body
/// no longer leaks the override into every subsequent test (Codex P2 #415).
pub(crate) struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    /// Set `key` to `value` for the lifetime of the returned guard.
    pub(crate) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }

    /// Remove `key` for the lifetime of the returned guard.
    pub(crate) fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}
