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
//!
//! ## Other process-global singletons
//!
//! The same "a module-local lock cannot serialise against another
//! module's lock" argument applies to any process-global mutable
//! singleton a test touches, not just env vars. [`GLOBAL_GUARD_LOCK`]
//! covers the injection-guard slot for that reason; see its docs.

use std::sync::Mutex;

/// The single crate-wide guard serialising env-mutating tests. See the module
/// docs for the soundness contract.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The single crate-wide guard serialising tests that install or clear
/// the process-global injection guard
/// ([`crate::hotkey::inject_guard::set_global`] /
/// [`crate::hotkey::inject_guard::global`]).
///
/// ## Why a crate-wide lock and not a module-local one
///
/// `set_global` is called from TWO places: directly by
/// `inject_guard`'s own tests, and indirectly by every
/// `crate::hotkey::install_hotkey` call — which several tests in
/// `hotkey/mod.rs` make. A lock private to `inject_guard_tests` would
/// serialise only the first group, so an `install_hotkey` test running
/// in parallel could replace the singleton between a
/// `set_global(g1)` / `set_global(g2)` pair and the `Arc::ptr_eq`
/// assertions that follow — making the last-writer-wins regression
/// test flaky. That is exactly the failure mode #668
///  3666165058 called out, and it is real even when the
/// interfering test's own headless listener startup later fails,
/// because `install_hotkey` publishes the guard BEFORE attempting
/// listener startup.
///
/// ## Usage rule
///
/// Every `#[test]` in the library that calls `set_global` /
/// `clear_global_for_tests` (directly) OR `install_hotkey` /
/// `install_hotkey_with_raw_tap` (which publish a guard internally)
/// MUST hold this lock for the duration of its global-guard
/// interaction.
pub(crate) static GLOBAL_GUARD_LOCK: Mutex<()> = Mutex::new(());

/// The single crate-wide guard serialising tests that read or mutate the
/// process-global whisper.cpp accelerator observer
/// ([`crate::whisper::accel::global`]).
///
/// ## Why a crate-wide lock and not a module-local one
///
/// Same argument as [`GLOBAL_GUARD_LOCK`]: the observer is written by
/// `accel`'s own tests and READ by unrelated modules' tests — the
/// `transcribe-server` response encoder stamps
/// `crate::whisper::accel::resolved_label()` onto every envelope, so
/// `whisper::protocol`'s tests assert on a value another module's test
/// can concurrently change. A lock private to either side would
/// serialise only that side.
///
/// ## Usage rule
///
/// Every `#[test]` in the library that calls `global().record(..)` /
/// `note_log_line(..)` / `set_planned(..)` / `reset()` on the global
/// observer, OR that asserts on a value derived from it
/// (`resolved_label()`, the `accel` field of a `TranscribeResponse`),
/// MUST hold this lock for the duration.
pub(crate) static ACCEL_OBSERVER_LOCK: Mutex<()> = Mutex::new(());
