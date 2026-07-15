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
//! Every `#[test]` in the library that calls `env::set_var` / `remove_var`
//! (directly or via a guard like `EnvVarGuard`) MUST hold [`ENV_LOCK`] across
//! the override/restore window. Re-export it from per-module `test_support`
//! shims rather than defining a new lock — historical per-module locks were
//! consolidated here exactly because they could not serialise against each
//! other.
//!
//! Integration tests in `tests/` live in their own binaries and so do not need
//! to share this lock with the library suite.

use std::sync::Mutex;

/// The single crate-wide guard serialising env-mutating tests. See the module
/// docs for the soundness contract.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
