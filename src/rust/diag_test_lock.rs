//! Process-wide serialisation for tests that mutate any part of
//! the diagnostic sink state — the writer slot **or** the level
//! atomic.
//!
//! Two independent pieces of process-global state interact at the
//! sink boundary in [`crate::diag::write_line`]:
//!
//! * The writer file installed by
//!   [`crate::diag::install_gui_diagnostic_log`] — the tempfile a
//!   `log!` call will land in.
//! * The `LogLevel` cached in the `LEVEL` atomic (mutated by
//!   [`crate::diag::init_from_env`] /
//!   [`crate::diag::reset_level_for_tests`]) — `Off` early-returns
//!   at the sink before the writer is even reached (the `#651`
//!   sink gate).
//!
//! Because `write_line` reads BOTH on every call, splitting the
//! two into separate locks lets a level-mutating test flip `LEVEL`
//! to `Off` mid-log for a writer-installing test in a different
//! module, and its expected write vanishes. That's exactly the
//! Codex flake Codex P2 #665 discussion PRRT_kwDOSfNjQs6UYXrm
//! flagged: an earlier version of this module scoped the lock to
//! "writer only", which left `init_from_env_reads_env_var_and_...`
//! (level-only) unsynchronised with
//! `log_macro_writes_prefixed_line_...` (writer-only).
//!
//! Historical background: the previous even-earlier version of the
//! serialisation was two function-local `OnceLock<Mutex<()>>` locks
//! named `diag_test_lock`, one in [`crate::diag_tests`] and one in
//! [`crate::hotkey::manager::tracker_tests`]. Those were TWO
//! DIFFERENT mutexes so Rust's parallel test runner could still
//! race the two suites' writer installs (Codex P2 #665 discussion
//! PRRT_kwDOSfNjQs6UYDJB — the first-round consolidation).
//!
//! **Usage rule:** every `#[test]` in the library that either
//! - installs a diagnostic writer
//!   ([`crate::diag::install_gui_diagnostic_log`]), **or**
//! - mutates the resolved log level
//!   ([`crate::diag::init_from_env`],
//!   [`crate::diag::reset_level_for_tests`], or setting
//!   `VOICEPI_LOG` and expecting the sink to observe it),
//!
//! MUST take [`DIAG_WRITER_LOCK`] across the install / mutate /
//! log / read window. Mirrors the [`crate::test_env_lock::ENV_LOCK`]
//! discipline for env-var mutation.
//!
//! Integration tests in `tests/` live in their own binaries and so
//! do not need to share this lock with the library suite.

use std::sync::Mutex;

/// The single crate-wide guard serialising tests that install a
/// diagnostic writer. See the module docs for the soundness
/// contract and the Codex P2 that motivated consolidating the two
/// historical per-module locks.
pub(crate) static DIAG_WRITER_LOCK: Mutex<()> = Mutex::new(());
