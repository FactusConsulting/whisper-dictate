//! Process-wide serialisation for tests that mutate the diagnostic
//! writer slot.
//!
//! [`crate::diag::install_gui_diagnostic_log`] swaps a
//! process-global `Option<File>` — every call replaces the file the
//! next `write_line` targets. Two parallel tests that both install to
//! their own tempfile would each see the other's writes land in the
//! wrong file (or the second install would silently redirect the
//! first test's later `log!` calls). Both symptoms manifest as
//! flaky, order-dependent test failures on the CI matrix.
//!
//! Historical bug: [`crate::diag_tests`] and
//! [`crate::hotkey::manager::tracker_tests`] each defined a
//! function-local `OnceLock<Mutex<()>>` named `diag_test_lock`, but
//! those were TWO DIFFERENT mutexes — one per compilation unit —
//! so Rust's parallel test runner could still race the two suites'
//! writer installs. Codex P2 #665 discussion PRRT_kwDOSfNjQs6UYDJB.
//!
//! Every test in the library that calls
//! [`crate::diag::install_gui_diagnostic_log`] (directly, or via a
//! helper) MUST take [`DIAG_WRITER_LOCK`] across the install /
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
