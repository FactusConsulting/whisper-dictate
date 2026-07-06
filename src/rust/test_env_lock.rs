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
///
/// **Poison handling.** Callers MUST acquire this lock with
/// `ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())` rather than `.unwrap()`
/// / `.expect(...)`: when any test panics inside the locked scope the mutex
/// is poisoned, and a bare `.unwrap()` turns that single real failure into
/// a `PoisonError { .. }` cascade across every subsequent env-mutating test
/// in the same library binary (Codex P2 #415 follow-up: this cascade was
/// the entire root cause of the rust (windows-2025) leg's 70+-test failure
/// set on PR #425). Recovering the inner value is safe because the env
/// state is restored by [`EnvVarGuard`]'s Drop pair regardless of whether
/// the test panicked, so a poisoned lock does not imply an inconsistent
/// env snapshot.
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
        debug_assert_lock_held();
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }

    /// Remove `key` for the lifetime of the returned guard.
    pub(crate) fn remove(key: &'static str) -> Self {
        debug_assert_lock_held();
        let original = env::var_os(key);
        env::remove_var(key);
        Self { key, original }
    }
}

/// Debug-only runtime check that the current thread holds [`ENV_LOCK`].
///
/// Type-system enforcement (taking `&MutexGuard` as a parameter to
/// every guard constructor) would catch the mistake at compile time
/// but would touch every adoption site; this `try_lock`-based probe is
/// the cheap backstop that catches a forgotten `ENV_LOCK.lock()` at
/// runtime in debug builds without disturbing the per-site call shape.
///
/// `try_lock` returns `Err(TryLockError::WouldBlock)` when *some* thread
/// holds the lock — that's the success case here (we don't separately
/// verify the holder is this thread; that would require `Mutex`'s
/// internal owner field which `std::sync::Mutex` doesn't expose). A
/// false-positive (another test holding the lock while this thread
/// forgot) is acceptable; the converse (we forgot AND nothing else is
/// holding) is the bug this firewall catches. The `Poisoned` arm is
/// treated as "lock IS held by a poisoned holder", which mirrors how
/// the rest of the module treats poison as recoverable.
fn debug_assert_lock_held() {
    debug_assert!(
        matches!(
            ENV_LOCK.try_lock(),
            Err(std::sync::TryLockError::WouldBlock) | Err(std::sync::TryLockError::Poisoned(_))
        ),
        "EnvVarGuard must be constructed while holding crate::test_env_lock::ENV_LOCK; \
         a missing `let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());` \
         lets the env mutation race other tests in the same library binary",
    );
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}
